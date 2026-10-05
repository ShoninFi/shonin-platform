//! The pro-rata fallback's upkeep and the restricted list, as anyone may
//! send them: a holder's units served from the station, a vault frozen for
//! units left unserved as long as a claim may wait, and a vault's restricted
//! list read again from the drivers the registry approves now.

use anchor_lang::{AccountDeserialize, Discriminator, InstructionData, ToAccountMetas};
use anyhow::{Context, Result};

use super::client::SolanaClient;
use super::platform::{Platform, VaultAccounts, event_authority};
use super::{AccountMeta, Instruction, Pubkey};

use shonin_registry::read as registry;
use shonin_registry::state::{ApprovalKind, ApprovalSlot, RevokedRecord};
use shonin_vault::state::{self as vs, Holder, Vault};

const SYSTEM: Pubkey = anchor_lang::solana_program::system_program::ID;

/// The accounts `refresh_restricted` reads one driver through: its approval
/// slot, the revocation record of the implementation that slot approves now,
/// and that implementation's program (the system program while none trades,
/// which the vault then skips without reading).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriverReading {
    pub slot: Pubkey,
    pub revoked: Pubkey,
    pub program: Pubkey,
}

impl Platform {
    /// Every holding of `vault` that holds units the fallback left, by owner.
    pub async fn unit_holders(&self, client: &SolanaClient, vault: &Pubkey) -> Result<Vec<Holder>> {
        let found = client
            .rpc
            .program_accounts(
                &shonin_vault::ID,
                &[
                    (0, Holder::DISCRIMINATOR.to_vec()),
                    (8, vault.to_bytes().to_vec()),
                ],
            )
            .await?;
        let mut holders = Vec::new();
        for (address, account) in found {
            let holder = Holder::try_deserialize(&mut &account.data[..])
                .context("a holding that doesn't read as one")?;
            // Only an account at the holding's own address is the holding.
            if holder.vault == *vault
                && holder.units != 0
                && vs::holder_address(vault, &holder.owner).0 == address
            {
                holders.push(holder);
            }
        }
        holders.sort_by_key(|holder| holder.owner);
        Ok(holders)
    }

    /// `serve_pro_rata`: exactly `owner`'s fraction of the station closed,
    /// and all it brings home credited to their units. Anyone may send it;
    /// the station's accounts are read from the cluster.
    pub async fn ix_serve_pro_rata(
        &self,
        client: &SolanaClient,
        v: &VaultAccounts,
        vault: &Vault,
        caller: &Pubkey,
        owner: &Pubkey,
    ) -> Result<Instruction> {
        let (groups, sizes) = self.groups(client, &v.station).await?;
        Ok(self.ix_serve_pro_rata_with(v, vault, caller, owner, groups, sizes))
    }

    /// [`Platform::ix_serve_pro_rata`] with the station's driver groups and
    /// their sizes in hand.
    pub fn ix_serve_pro_rata_with(
        &self,
        v: &VaultAccounts,
        vault: &Vault,
        caller: &Pubkey,
        owner: &Pubkey,
        groups: Vec<AccountMeta>,
        sizes: Vec<u8>,
    ) -> Instruction {
        let mut accounts = shonin_vault::accounts::ServeProRata {
            caller: *caller,
            pool: self.pool(v, vault),
            close: shonin_vault::accounts::Closing {
                station_program: shonin_station::ID,
                station_dollars: v.station_dollars,
            },
            holder: vs::holder_address(&v.vault, owner).0,
            event_authority: event_authority(),
            program: shonin_vault::ID,
        }
        .to_account_metas(None);
        accounts.extend(groups);
        Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data: shonin_vault::instruction::ServeProRata {
                owner: *owner,
                groups: sizes,
            }
            .data(),
        }
    }

    /// `freeze_units`: freeze a vault whose `owner`'s units have waited
    /// unserved as long as a claim may.
    pub fn ix_freeze_units(&self, vault: &Pubkey, owner: &Pubkey) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::FreezeUnits {
                vault: *vault,
                holder: vs::holder_address(vault, owner).0,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::FreezeUnits {}.data(),
        }
    }

    /// How `refresh_restricted` reads the driver named `name` at `now`: the
    /// implementation its registry slot approves now, unless that one is
    /// revoked.
    pub async fn driver_reading(
        &self,
        client: &SolanaClient,
        name: &[u8; 32],
        now: u64,
    ) -> Result<DriverReading> {
        let slot_address = registry::approval_address(ApprovalKind::Driver, name);
        let slot = client.load::<ApprovalSlot>(&slot_address).await?;
        let effective = slot
            .as_ref()
            .map(|slot| slot.effective(now))
            .unwrap_or_default();
        let revoked = registry::revoked_address(ApprovalKind::Driver, name, &effective);
        let record = client.load::<RevokedRecord>(&revoked).await?;
        let trading = shonin_registry::state::trading(slot.as_ref(), record.as_ref(), now);
        Ok(DriverReading {
            slot: slot_address,
            revoked,
            program: if trading == Pubkey::default() {
                SYSTEM
            } else {
                trading
            },
        })
    }

    /// `refresh_restricted`: the vault's list read again from the drivers
    /// named, each by its place in the vault and how it is read. `payer`
    /// pays for any room the longer list needs.
    pub fn ix_refresh_restricted(
        &self,
        vault: &Pubkey,
        payer: &Pubkey,
        drivers: &[(u8, DriverReading)],
    ) -> Instruction {
        let mut accounts = shonin_vault::accounts::RefreshRestricted {
            payer: *payer,
            vault: *vault,
            restricted: vs::restricted_address(vault).0,
            system_program: SYSTEM,
        }
        .to_account_metas(None);
        for (_, reading) in drivers {
            accounts.extend([
                AccountMeta::new_readonly(reading.slot, false),
                AccountMeta::new_readonly(reading.revoked, false),
                AccountMeta::new_readonly(reading.program, false),
            ]);
        }
        Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data: shonin_vault::instruction::RefreshRestricted {
                drivers: drivers.iter().map(|(index, _)| *index).collect(),
            }
            .data(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform() -> Platform {
        Platform {
            cluster_name: "localnet".into(),
            cluster: [0; 32],
            mint: Pubkey::new_unique(),
            admin: Pubkey::new_unique(),
            platform: Pubkey::new_unique(),
            operator: Pubkey::new_unique(),
            checker: None,
        }
    }

    fn a_vault(author: Pubkey, platform: Pubkey) -> Vault {
        let mut vault = Vault::try_deserialize_unchecked(&mut &[0u8; 8192][..]).unwrap();
        vault.author = author;
        vault.platform = platform;
        vault
    }

    #[test]
    fn a_serve_names_the_holding_of_the_owner_served_and_carries_the_station_s_groups() {
        let p = platform();
        let v = p.vault_accounts(7);
        let vault = a_vault(Pubkey::new_unique(), p.platform);
        let (caller, owner) = (Pubkey::new_unique(), Pubkey::new_unique());
        let group = vec![
            AccountMeta::new_readonly(Pubkey::new_unique(), false),
            AccountMeta::new(Pubkey::new_unique(), false),
        ];
        let ix = p.ix_serve_pro_rata_with(&v, &vault, &caller, &owner, group.clone(), vec![2]);
        assert_eq!(ix.program_id, shonin_vault::ID);
        assert_eq!(
            ix.data,
            shonin_vault::instruction::ServeProRata {
                owner,
                groups: vec![2]
            }
            .data()
        );
        assert_eq!(ix.accounts[0], AccountMeta::new_readonly(caller, true));
        let holder = vs::holder_address(&v.vault, &owner).0;
        assert!(
            ix.accounts
                .iter()
                .any(|meta| meta.pubkey == holder && meta.is_writable),
            "the served owner's holding is not named, writable"
        );
        assert!(ix.accounts.iter().any(|meta| meta.pubkey == v.station));
        assert!(
            ix.accounts
                .iter()
                .any(|meta| meta.pubkey == v.station_dollars && meta.is_writable)
        );
        assert_eq!(ix.accounts[ix.accounts.len() - 2..], group[..]);
    }

    #[test]
    fn a_freeze_for_units_names_the_vault_and_the_unserved_owner_s_holding() {
        let p = platform();
        let (vault, owner) = (Pubkey::new_unique(), Pubkey::new_unique());
        let ix = p.ix_freeze_units(&vault, &owner);
        assert_eq!(ix.data, shonin_vault::instruction::FreezeUnits {}.data());
        assert_eq!(
            ix.accounts,
            vec![
                AccountMeta::new(vault, false),
                AccountMeta::new_readonly(vs::holder_address(&vault, &owner).0, false),
            ]
        );
    }

    #[test]
    fn a_refresh_reads_each_driver_named_through_its_three_accounts_in_order() {
        let p = platform();
        let (vault, payer) = (Pubkey::new_unique(), Pubkey::new_unique());
        let reading = || DriverReading {
            slot: Pubkey::new_unique(),
            revoked: Pubkey::new_unique(),
            program: Pubkey::new_unique(),
        };
        let (first, second) = (reading(), reading());
        let ix = p.ix_refresh_restricted(&vault, &payer, &[(2, first), (0, second)]);
        assert_eq!(
            ix.data,
            shonin_vault::instruction::RefreshRestricted {
                drivers: vec![2, 0]
            }
            .data()
        );
        assert_eq!(ix.accounts[0], AccountMeta::new(payer, true));
        assert_eq!(ix.accounts[1], AccountMeta::new(vault, false));
        assert_eq!(
            ix.accounts[2],
            AccountMeta::new(vs::restricted_address(&vault).0, false)
        );
        assert_eq!(ix.accounts[3].pubkey, SYSTEM);
        let read: Vec<Pubkey> = ix.accounts[4..].iter().map(|meta| meta.pubkey).collect();
        assert_eq!(
            read,
            [
                first.slot,
                first.revoked,
                first.program,
                second.slot,
                second.revoked,
                second.program
            ]
        );
        assert!(ix.accounts[4..].iter().all(|meta| !meta.is_writable));
    }
}
