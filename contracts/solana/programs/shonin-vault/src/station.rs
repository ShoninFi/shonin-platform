//! Calling the vault's station.
//!
//! The vault reaches its station only by calling the station program, signing
//! as the vault's own address where the station asks the vault to sign. The
//! station answers through return data. It never calls the vault back while
//! the vault is calling it (the runtime forbids it), so what a call brought
//! home is measured by the rise in the station's running total of what it has
//! sent home, read after the call.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::{get_return_data, invoke, invoke_signed, set_return_data},
};
use anchor_lang::{InstructionData, ToAccountMetas};
use shonin_driver::Valuation;
use shonin_station::state::Outcome;

use crate::error::VaultError;

/// The accounts every call through the station's money paths names.
pub struct Station<'a, 'info> {
    pub program: &'a AccountInfo<'info>,
    pub station: &'a AccountInfo<'info>,
    pub vault: &'a AccountInfo<'info>,
    pub seeds: &'a [&'a [u8]],
}

/// Marks the vault as the call's signer: the vault program signs for it.
fn signed_by(mut accounts: Vec<AccountMeta>, vault: &Pubkey) -> Vec<AccountMeta> {
    for meta in accounts.iter_mut() {
        meta.is_signer = meta.pubkey == *vault;
    }
    accounts
}

fn metas(accounts: &[AccountInfo]) -> Vec<AccountMeta> {
    accounts
        .iter()
        .map(|info| AccountMeta {
            pubkey: *info.key,
            is_signer: false,
            is_writable: info.is_writable,
        })
        .collect()
}

fn answer<T: AnchorDeserialize>() -> Result<T> {
    let (from, data) = get_return_data().ok_or(VaultError::StationSilent)?;
    require_keys_eq!(from, shonin_station::ID, VaultError::StationSilent);
    T::try_from_slice(&data).map_err(|_| error!(VaultError::StationSilent))
}

/// What the station holds, at mark and at exit-side prices, its counted
/// dollars included, and whether any market it holds is outside the band.
/// `groups` are its drivers' accounts, one group per pinned driver.
pub fn value<'info>(
    program: &AccountInfo<'info>,
    station: &AccountInfo<'info>,
    groups: &[AccountInfo<'info>],
    sizes: Vec<u8>,
) -> Result<Valuation> {
    require_keys_eq!(*program.key, shonin_station::ID, VaultError::WrongAccount);
    let mut accounts = shonin_station::accounts::Value {
        station: *station.key,
    }
    .to_account_metas(None);
    accounts.extend(metas(groups));
    let ix = Instruction {
        program_id: shonin_station::ID,
        accounts,
        data: shonin_station::instruction::Value { groups: sizes }.data(),
    };
    let mut infos = vec![station.clone()];
    infos.extend(groups.iter().cloned());
    infos.push(program.clone());
    set_return_data(&[]);
    invoke(&ix, &infos)?;
    answer()
}

impl<'info> Station<'_, 'info> {
    /// Closes `fraction` of every position inside the exit band and has its
    /// worth, and that fraction of the station's counted dollars, sent home.
    #[allow(clippy::too_many_arguments)]
    pub fn unwind(
        &self,
        station_dollars: &AccountInfo<'info>,
        vault_dollars: &AccountInfo<'info>,
        mint: &AccountInfo<'info>,
        token_program: &AccountInfo<'info>,
        groups: &[AccountInfo<'info>],
        sizes: Vec<u8>,
        fraction: u64,
    ) -> Result<Outcome> {
        require_keys_eq!(
            *self.program.key,
            shonin_station::ID,
            VaultError::WrongAccount
        );
        let accounts = shonin_station::accounts::Unwind {
            caller: *self.vault.key,
            station: *self.station.key,
            station_dollars: *station_dollars.key,
            vault_dollars: *vault_dollars.key,
            mint: *mint.key,
            token_program: *token_program.key,
            registry_config: None,
            blessing: None,
            key_info: None,
            image: None,
            project: None,
            vault: None,
            vault_program: None,
        }
        .to_account_metas(None);
        let mut accounts = signed_by(accounts, self.vault.key);
        accounts.extend(metas(groups));
        let ix = Instruction {
            program_id: shonin_station::ID,
            accounts,
            data: shonin_station::instruction::Unwind {
                fraction,
                groups: sizes,
            }
            .data(),
        };
        let mut infos = vec![
            self.vault.clone(),
            self.station.clone(),
            station_dollars.clone(),
            vault_dollars.clone(),
            mint.clone(),
            token_program.clone(),
        ];
        infos.extend(groups.iter().cloned());
        infos.push(self.program.clone());
        set_return_data(&[]);
        invoke_signed(&ix, &infos, &[self.seeds])?;
        answer()
    }

    /// Sends `amount` of the vault's dollars to the station.
    pub fn pull(
        &self,
        vault_dollars: &AccountInfo<'info>,
        station_dollars: &AccountInfo<'info>,
        mint: &AccountInfo<'info>,
        token_program: &AccountInfo<'info>,
        amount: u64,
    ) -> Result<()> {
        require_keys_eq!(
            *self.program.key,
            shonin_station::ID,
            VaultError::WrongAccount
        );
        let ix = Instruction {
            program_id: shonin_station::ID,
            accounts: signed_by(
                shonin_station::accounts::Pull {
                    vault: *self.vault.key,
                    station: *self.station.key,
                    vault_dollars: *vault_dollars.key,
                    station_dollars: *station_dollars.key,
                    mint: *mint.key,
                    token_program: *token_program.key,
                }
                .to_account_metas(None),
                self.vault.key,
            ),
            data: shonin_station::instruction::Pull { amount }.data(),
        };
        let infos = [
            self.vault.clone(),
            self.station.clone(),
            vault_dollars.clone(),
            station_dollars.clone(),
            mint.clone(),
            token_program.clone(),
            self.program.clone(),
        ];
        invoke_signed(&ix, &infos, &[self.seeds])?;
        Ok(())
    }

    /// Has the station send `amount` of its counted dollars home.
    pub fn push(
        &self,
        vault_dollars: &AccountInfo<'info>,
        station_dollars: &AccountInfo<'info>,
        mint: &AccountInfo<'info>,
        token_program: &AccountInfo<'info>,
        amount: u64,
    ) -> Result<()> {
        require_keys_eq!(
            *self.program.key,
            shonin_station::ID,
            VaultError::WrongAccount
        );
        let ix = Instruction {
            program_id: shonin_station::ID,
            accounts: signed_by(
                shonin_station::accounts::Push {
                    vault: *self.vault.key,
                    station: *self.station.key,
                    vault_dollars: *vault_dollars.key,
                    station_dollars: *station_dollars.key,
                    mint: *mint.key,
                    token_program: *token_program.key,
                }
                .to_account_metas(None),
                self.vault.key,
            ),
            data: shonin_station::instruction::Push { amount }.data(),
        };
        let infos = [
            self.vault.clone(),
            self.station.clone(),
            vault_dollars.clone(),
            station_dollars.clone(),
            mint.clone(),
            token_program.clone(),
            self.program.clone(),
        ];
        invoke_signed(&ix, &infos, &[self.seeds])?;
        Ok(())
    }
}

/// A driver's restricted list, asked of its program directly: a list only,
/// with no accounts and nothing signed.
pub fn restricted_of(driver: &AccountInfo) -> Result<Vec<[u8; 32]>> {
    let ix = Instruction {
        program_id: *driver.key,
        accounts: vec![],
        data: shonin_driver::discriminator::RESTRICTED.to_vec(),
    };
    set_return_data(&[]);
    invoke(&ix, std::slice::from_ref(driver))?;
    let (from, data) = get_return_data().ok_or(VaultError::StationSilent)?;
    require_keys_eq!(from, *driver.key, VaultError::StationSilent);
    Vec::<[u8; 32]>::try_from_slice(&data).map_err(|_| error!(VaultError::StationSilent))
}
