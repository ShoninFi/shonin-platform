//! What keeps a vault going, as anyone may send it: the bill collected with
//! the executor's signed running total, rent taken back from finished
//! requests and claims, held payouts collected, a closed vault's doors, the
//! freeze, the eviction, the close in one transaction, and finding every
//! vault, request and claim the vault program holds.

use anchor_lang::{AccountDeserialize, Discriminator, InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use anyhow::{Context, Result, anyhow};

use super::client::SolanaClient;
use super::platform::{Platform, VaultAccounts};
use super::{Instruction, Pubkey};

use shonin_registry::read as registry;
use shonin_vault::state::{self as vs, Claim, Request, RequestKind, Vault, VaultState};
use shonin_vault::upkeep::{HeldIn, Usage};

const SYSTEM: Pubkey = anchor_lang::solana_program::system_program::ID;
const INSTRUCTIONS: Pubkey = anchor_lang::solana_program::sysvar::instructions::ID;

/// A running usage total as the vault's executor signed it: the total, when
/// it was signed, the key that signed and its ed25519 signature over
/// [`shonin_vault::upkeep::usage_digest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedUsage {
    pub total: u64,
    pub as_of: u64,
    pub signer: Pubkey,
    pub signature: [u8; 64],
}

impl SignedUsage {
    /// What the signature is over, for `vault` on the cluster `cluster`
    /// stands for.
    pub fn digest(&self, cluster: &[u8; 32], vault: &Pubkey) -> [u8; 32] {
        shonin_vault::upkeep::usage_digest(cluster, vault, self.total, self.as_of)
    }
}

/// An Ed25519 instruction carrying a signature someone else made over
/// `message`: how the executor's signed total travels in the keeper's
/// transaction.
pub fn ed25519_signed(signer: &Pubkey, signature: &[u8; 64], message: &[u8]) -> Instruction {
    solana_ed25519_program::new_ed25519_instruction_with_signature(
        message,
        signature,
        &signer.to_bytes(),
    )
}

impl Platform {
    // Finding what the vault program holds ---------------------------------------------

    /// Every vault the vault program holds, with its account as it is now.
    pub async fn vaults(&self, client: &SolanaClient) -> Result<Vec<(Pubkey, Vault)>> {
        let found = client
            .rpc
            .program_accounts(&shonin_vault::ID, &[(0, Vault::DISCRIMINATOR.to_vec())])
            .await?;
        let mut vaults = Vec::new();
        for (address, account) in found {
            let vault = Vault::try_deserialize(&mut &account.data[..])
                .map_err(|error| anyhow!("the vault at {address} doesn't read as one: {error}"))?;
            // Only an account at the vault's own address is the vault.
            if vs::vault_address(vault.strategy_id).0 == address {
                vaults.push((address, vault));
            }
        }
        vaults.sort_by_key(|(_, vault)| vault.strategy_id);
        Ok(vaults)
    }

    /// Every request account of `vault`, wherever its maker put it, by id.
    pub async fn requests_of(
        &self,
        client: &SolanaClient,
        vault: &Pubkey,
    ) -> Result<Vec<(Pubkey, Request)>> {
        let found = client
            .rpc
            .program_accounts(
                &shonin_vault::ID,
                &[
                    (0, Request::DISCRIMINATOR.to_vec()),
                    (8, vault.to_bytes().to_vec()),
                ],
            )
            .await?;
        let mut requests = Vec::new();
        for (address, account) in found {
            let request = Request::try_deserialize(&mut &account.data[..])
                .context("a request account that doesn't read as one")?;
            if request.vault == *vault {
                requests.push((address, request));
            }
        }
        requests.sort_by_key(|(_, request)| request.id);
        Ok(requests)
    }

    /// Every claim of the vaults named, by vault, each at its own address. A
    /// claim doesn't name its vault, so it is matched by address.
    pub async fn claims_of(
        &self,
        client: &SolanaClient,
        vaults: &[Pubkey],
    ) -> Result<Vec<(Pubkey, Pubkey, Claim)>> {
        let found = client
            .rpc
            .program_accounts(&shonin_vault::ID, &[(0, Claim::DISCRIMINATOR.to_vec())])
            .await?;
        let mut claims = Vec::new();
        for (address, account) in found {
            let claim = Claim::try_deserialize(&mut &account.data[..])
                .context("a claim account that doesn't read as one")?;
            if let Some(vault) = vaults
                .iter()
                .find(|vault| vs::claim_address(vault, claim.id).0 == address)
            {
                claims.push((*vault, address, claim));
            }
        }
        claims.sort_by_key(|(vault, _, claim)| (*vault, claim.id));
        Ok(claims)
    }

    // The bill ----------------------------------------------------------------------------

    /// `keep`: with a signed total, the bill collected from the tank (the
    /// Ed25519 instruction carrying the signature comes first); either way,
    /// a fill of the tank queued if the author keeps it above what it holds.
    /// `caller` pays the rent of a fill's request.
    pub async fn ix_keep(
        &self,
        client: &SolanaClient,
        v: &VaultAccounts,
        vault: &Vault,
        caller: &Pubkey,
        signed: Option<&SignedUsage>,
    ) -> Result<Vec<Instruction>> {
        // Without a total the billing accounts are not read: the config
        // stands in for each.
        let config = registry::config_address();
        let [registry_config, blessing, key_info, image, project] = match signed {
            Some(signed) => {
                self.key_records(client, &v.vault, &vault.bundle_hash, &signed.signer)
                    .await?
            }
            None => [config; 5],
        };
        let keep = Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::Keep {
                caller: *caller,
                vault: v.vault,
                request: Some(vs::request_address(&v.vault, &v.vault, vault.tank.fill_request).0),
                billing: shonin_vault::accounts::Billing {
                    registry_config,
                    blessing,
                    key_info,
                    image,
                    project,
                    prices: shonin_prices::read::prices_address(),
                    instructions: INSTRUCTIONS,
                    platform_dollars: self.dollars(&vault.platform),
                },
                vault_dollars: v.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::Keep {
                usage: signed.map(|signed| Usage {
                    total: signed.total,
                    as_of: signed.as_of,
                    signer: signed.signer,
                }),
            }
            .data(),
        };
        // The signature is checked over the cluster this client connected
        // to and checked, not the one the node says the vault names: the
        // vault program refuses it if they differ.
        Ok(match signed {
            Some(signed) => vec![
                ed25519_signed(
                    &signed.signer,
                    &signed.signature,
                    &signed.digest(&client.cluster_value(), &v.vault),
                ),
                keep,
            ],
            None => vec![keep],
        })
    }

    // Rent back -----------------------------------------------------------------------------

    /// Close a finished request, its rent going back to whoever paid it.
    pub fn ix_close_request(
        &self,
        vault: &Pubkey,
        record: &Pubkey,
        request: &Request,
    ) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::CloseRecord {
                vault: *vault,
                record: *record,
                payer: request.payer,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::CloseRequest {
                request_id: request.id,
            }
            .data(),
        }
    }

    /// Close a paid claim, its rent going back to whoever paid it.
    pub fn ix_close_claim(&self, vault: &Pubkey, claim: &Claim) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::CloseRecord {
                vault: *vault,
                record: vs::claim_address(vault, claim.id).0,
                payer: claim.payer,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::CloseClaim { claim_id: claim.id }.data(),
        }
    }

    /// Whether a request's account may be closed now: settled or taken, with
    /// nothing held on it, and passed by settlement (or the vault closed).
    pub fn request_closable(vault: &Vault, request: &Request) -> bool {
        record_closable(
            request.done,
            request.held,
            request.id,
            vault.request_cursor,
            vault.state == VaultState::Closed,
        )
    }

    /// The same for a claim: paid or taken through the fallback, holding
    /// nothing, and passed by settlement (or the vault closed).
    pub fn claim_closable(vault: &Vault, claim: &Claim) -> bool {
        use shonin_vault::state::ClaimState;
        record_closable(
            matches!(claim.state, ClaimState::Paid | ClaimState::Fallback),
            claim.held,
            claim.id,
            vault.request_cursor,
            vault.state == VaultState::Closed,
        )
    }

    // Held payouts ----------------------------------------------------------------------------

    /// `owner` takes a payout held for them into their dollar account `to`:
    /// one held on a request or a claim (whose account is `record`), or a
    /// claim's fee held for the author or the platform (no record).
    pub fn ix_collect_held(
        &self,
        v: &VaultAccounts,
        owner: &Pubkey,
        held: HeldIn,
        record: Option<Pubkey>,
        to: &Pubkey,
    ) -> Instruction {
        // A claim's account is at the vault's address for it; a request's is
        // its maker's (see [`Platform::find_request`]).
        let record = match held {
            HeldIn::Claim(id) => Some(record.unwrap_or(vs::claim_address(&v.vault, id).0)),
            HeldIn::Request(_) => record,
            HeldIn::Fees => None,
        };
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::CollectHeld {
                owner: *owner,
                vault: v.vault,
                record,
                to: *to,
                vault_dollars: v.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::CollectHeld { held }.data(),
        }
    }

    // The lifecycle -----------------------------------------------------------------------------

    /// Freeze a vault whose claim has gone unpaid too long.
    pub fn ix_freeze(&self, vault: &Pubkey, claim_id: u64) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::Freeze {
                vault: *vault,
                claim: vs::claim_address(vault, claim_id).0,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::Freeze { claim_id }.data(),
        }
    }

    /// Halt a retired vault whose wind-down is over.
    pub fn ix_evict(&self, vault: &Pubkey) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::Anyone { vault: *vault }.to_account_metas(None),
            data: shonin_vault::instruction::Evict {}.data(),
        }
    }

    /// A closed vault settles a request that was still waiting when it
    /// closed: a deposit goes back to its payer, a redemption is paid its
    /// share of the cash.
    pub fn ix_settle_closed(
        &self,
        v: &VaultAccounts,
        vault: &Vault,
        record: &Pubkey,
        request: &Request,
    ) -> Instruction {
        let (other, to) = match request.kind {
            RequestKind::Deposit => (
                Some(vs::holder_address(&v.vault, &request.owner).0),
                self.dollars(&request.payer),
            ),
            RequestKind::Redeem => (None, self.dollars(&request.owner)),
            RequestKind::Claim | RequestKind::TankFill => (None, self.dollars(&vault.author)),
        };
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::ClosedDoor {
                vault: v.vault,
                station: v.station,
                record: *record,
                other,
                to,
                vault_dollars: v.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::SettleClosed {
                request_id: request.id,
            }
            .data(),
        }
    }

    /// A closed vault pays a claim still waiting when it closed. A queued
    /// claim names the request it is queued by, `queued_by`: found among the
    /// vault's requests by [`queued_request`], as the program finds it by
    /// its id.
    pub fn ix_claim_closed(
        &self,
        v: &VaultAccounts,
        claim: &Claim,
        queued_by: Option<&Pubkey>,
    ) -> Instruction {
        let address = vs::claim_address(&v.vault, claim.id).0;
        let other = queued_by.copied();
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::ClosedDoor {
                vault: v.vault,
                station: v.station,
                record: address,
                other,
                to: self.dollars(&claim.owner),
                vault_dollars: v.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::ClaimClosed { claim_id: claim.id }.data(),
        }
    }

    /// A halted vault closed in one transaction, which nobody can get
    /// between: its station stopped (unless it is), unwound in full, the
    /// vault valued and closed. Only when nothing a valuation priced is
    /// still owed: settlement can't share the transaction.
    pub async fn ixs_close_in_one(
        &self,
        client: &SolanaClient,
        v: &VaultAccounts,
        vault: &Vault,
        caller: &Pubkey,
        stopped: bool,
    ) -> Result<Vec<Instruction>> {
        let mut ixs = Vec::new();
        if !stopped {
            ixs.push(self.ix_stop(v));
        }
        ixs.push(
            self.ix_unwind(client, v, caller, shonin_driver::WAD)
                .await?,
        );
        ixs.push(self.ix_record_valuation(client, v).await?);
        ixs.push(self.ix_close_vault(v, vault));
        Ok(ixs)
    }
}

/// Where the request a queued claim is queued by lives, among `requests`
/// (the vault's, with their addresses). The claim's `request` field holds
/// that request's queue id, and the program finds it by that id: it made
/// it at the claim's request address for the nonce the claim held *before*
/// (0 the first time), so it is not at the address the id would give. None
/// for a claim not queued, or whose request isn't among them.
pub fn queued_request(
    vault: &Pubkey,
    claim: &Claim,
    requests: &[(Pubkey, Request)],
) -> Option<Pubkey> {
    if claim.state != shonin_vault::state::ClaimState::Queued {
        return None;
    }
    requests
        .iter()
        .find(|(_, r)| {
            r.vault == *vault
                && r.id == claim.request
                && r.kind == RequestKind::Claim
                && r.reference == claim.id
                && r.owner == claim.owner
        })
        .map(|(address, _)| *address)
}

/// Whether a request or claim account may be closed for its rent: finished,
/// holding nothing, and no longer needed by settlement (its id passed, or the
/// vault closed). The vault's own `close_request` and `close_claim` rule.
fn record_closable(finished: bool, held: u64, id: u64, cursor: u64, closed: bool) -> bool {
    finished && held == 0 && (id <= cursor || closed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solana::{Keypair, Signer};

    #[test]
    fn a_signed_total_travels_as_the_signer_s_own_signature_over_the_vault_s_digest() {
        let executor = Keypair::new();
        let vault = Pubkey::new_unique();
        let cluster = shonin_common::digest::cluster_value("localnet");
        let digest = shonin_vault::upkeep::usage_digest(&cluster, &vault, 1_234, 99);
        let signature: [u8; 64] = executor.sign_message(&digest).as_ref().try_into().unwrap();
        let signed = SignedUsage {
            total: 1_234,
            as_of: 99,
            signer: executor.pubkey(),
            signature,
        };
        assert_eq!(signed.digest(&cluster, &vault), digest);
        assert_ne!(signed.digest(&cluster, &Pubkey::new_unique()), digest);
        let ix = ed25519_signed(&signed.signer, &signature, &digest);
        assert_eq!(
            ix.program_id.to_string(),
            "Ed25519SigVerify111111111111111111111111111"
        );
        // The instruction carries the key, the signature and the message.
        assert!(
            ix.data
                .windows(32)
                .any(|w| w == executor.pubkey().to_bytes())
        );
        assert!(ix.data.windows(32).any(|w| w == digest));
        assert!(ix.data.windows(64).any(|w| w == signature));
    }

    #[test]
    fn a_record_is_closable_only_once_finished_holding_nothing_and_passed() {
        assert!(record_closable(true, 0, 5, 5, false));
        assert!(!record_closable(true, 0, 6, 5, false), "not passed yet");
        assert!(record_closable(true, 0, 6, 5, true), "the vault closed");
        assert!(!record_closable(true, 1, 5, 5, true), "holds a payout");
        assert!(!record_closable(false, 0, 5, 5, true), "not finished");
    }
}
