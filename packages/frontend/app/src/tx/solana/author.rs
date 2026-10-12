//! The author's flows on a Solana vault: the tank (fund it, top it up from
//! fee shares, the level it is kept at, the unsigned escape and the signed
//! withdrawal), the allowances, the idle floor, and retiring. Each is one
//! transaction through [`Ctx::transact`], the same path as the depositor's.
//!
//! Activation and the publishing payment on Solana are not here: see the
//! module's notes in INTERFACES (they need the registry's driver approvals
//! and the factory's earlier programs read in the browser).

use chain::solana_ix::accounts;
use chain::solana_ix::vault::{self as ix, Allowances, SignedUsage};
use chain::solana_ix::{Pubkey, address};

use super::error::{Reason, SolError};
use super::flow::{Ctx, Loaded, Outcome, Prompt, Step, holder};
use super::port::{SolReader, SolSigner};
use crate::tx::port::Clock;

/// What the author's wallet is asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorPrompt {
    FundTank { assets: u64 },
    TopUpFromShares { shares: u128 },
    SetKeep { assets: u64 },
    RequestEscape,
    WithdrawTank { assets: u64 },
    Allowances,
    IdleFloor { bps: u16 },
    Retire,
}

/// One author action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorAction {
    /// Put the connected wallet's dollars into the tank (anyone may).
    FundTank(u64),
    /// Redeem the author's own shares into the tank.
    TopUpFromShares(u128),
    /// Keep the tank at this level from fee shares; zero turns it off.
    SetKeep(u64),
    /// Ask to withdraw the tank without a signed total.
    RequestEscape,
    /// Withdraw from the tank: with the executor's signed total when given,
    /// else through the escape once it is open.
    WithdrawTank {
        assets: u64,
        signed: Option<SignedUsage>,
    },
    SetAllowances(Allowances),
    SetIdleFloor(u16),
    Retire,
}

fn not_allowed() -> SolError {
    SolError::WouldFail {
        reason: Reason::Vault("NotAllowed"),
    }
}

/// Run one author action on the vault.
pub async fn run<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    action: AuthorAction,
) -> Result<Outcome, SolError> {
    ctx.step(Step::Checking);
    ctx.check_endpoint().await?;
    let Loaded { vault, keys } = ctx.load().await?;
    let is_author = vault.author == ctx.account;
    // Everything but funding the tank is the author's alone.
    if !is_author && !matches!(action, AuthorAction::FundTank(_)) {
        return Err(not_allowed());
    }
    let mut ixs = Vec::new();
    let prompt = match action {
        AuthorAction::FundTank(assets) => {
            if assets == 0 {
                return Err(SolError::NothingToSend);
            }
            let have = ctx.dollars_of(&ctx.account).await?;
            if have < assets {
                return Err(SolError::InsufficientBalance { have });
            }
            ixs.push(ix::fund_tank(&keys, &ctx.account, assets));
            AuthorPrompt::FundTank { assets }
        }
        AuthorAction::TopUpFromShares(shares) => {
            if shares == 0 {
                return Err(SolError::NothingToSend);
            }
            let held = holder(ctx.reader, &ctx.vault, &ctx.account)
                .await?
                .map_or(0, |holder| holder.shares);
            if held < shares {
                return Err(SolError::InsufficientShares { have: held });
            }
            let nonce = ctx.nonce(&vault, &vault.author).await?;
            ixs.push(ix::top_up_from_shares(&keys, shares, nonce));
            AuthorPrompt::TopUpFromShares { shares }
        }
        AuthorAction::SetKeep(assets) => {
            ixs.push(ix::set_tank_keep(&keys, assets));
            AuthorPrompt::SetKeep { assets }
        }
        AuthorAction::RequestEscape => {
            ixs.push(ix::request_tank_withdrawal(&keys));
            AuthorPrompt::RequestEscape
        }
        AuthorAction::WithdrawTank { assets, signed } => {
            if assets == 0 {
                return Err(SolError::NothingToSend);
            }
            ctx.open_if_missing(&vault.author, &mut ixs).await?;
            match signed {
                Some(signed) => {
                    let info = ctx
                        .reader
                        .account(&address::key(&signed.signer))
                        .await?
                        .ok_or(not_allowed())?;
                    let records = accounts::key_records(
                        &signed.signer,
                        &info.data,
                        &vault.cluster,
                        &ctx.vault,
                        &vault.bundle_hash,
                    )
                    .ok_or(not_allowed())?;
                    ixs.extend(ix::withdraw_tank(
                        &keys,
                        assets,
                        &signed,
                        &records,
                        &vault.cluster,
                    ));
                }
                None => ixs.push(ix::withdraw_tank_unsigned(&keys, assets)),
            }
            AuthorPrompt::WithdrawTank { assets }
        }
        AuthorAction::SetAllowances(allowances) => {
            ixs.push(ix::set_allowances(&keys, &allowances));
            AuthorPrompt::Allowances
        }
        AuthorAction::SetIdleFloor(bps) => {
            if bps > 10_000 {
                return Err(SolError::WouldFail {
                    reason: Reason::Vault("OutOfRange"),
                });
            }
            ixs.push(ix::set_idle_floor(&keys, bps));
            AuthorPrompt::IdleFloor { bps }
        }
        AuthorAction::Retire => {
            if vault.state != accounts::VaultState::Open {
                return Err(SolError::WrongState);
            }
            ixs.push(ix::retire(&keys));
            AuthorPrompt::Retire
        }
    };
    let landed = ctx.transact(Prompt::Author(prompt), ixs).await?;
    Ok(Outcome::Changed {
        signature: landed.signature,
    })
}

/// A usage total the vault's executor signed, as the API serves it at
/// `/api/solana/usage/{vault}`: `None` if it isn't for `vault` or doesn't
/// read.
pub fn signed_usage(value: &serde_json::Value, vault: &Pubkey) -> Option<SignedUsage> {
    if value["vault"].as_str()? != vault.to_string() {
        return None;
    }
    let total = match &value["total"] {
        serde_json::Value::String(text) => text.parse().ok()?,
        other => other.as_u64()?,
    };
    let signature: [u8; 64] = bs58::decode(value["signature"].as_str()?)
        .into_vec()
        .ok()?
        .try_into()
        .ok()?;
    Some(SignedUsage {
        total,
        as_of: value["as_of"].as_u64()?,
        signer: value["signer"].as_str()?.parse().ok()?,
        signature,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_total_is_read_only_for_its_own_vault() {
        let vault = Pubkey::new_from_array([1; 32]);
        let signer = Pubkey::new_from_array([2; 32]);
        let value = serde_json::json!({
            "vault": vault.to_string(),
            "total": "3077",
            "as_of": 1_811_442_100u64,
            "signer": signer.to_string(),
            "signature": bs58::encode([9u8; 64]).into_string(),
        });
        let signed = signed_usage(&value, &vault).unwrap();
        assert_eq!(signed.total, 3077);
        assert_eq!(signed.as_of, 1_811_442_100);
        assert_eq!(signed.signer, signer);
        assert_eq!(signed.signature, [9; 64]);
        assert!(signed_usage(&value, &Pubkey::new_from_array([3; 32])).is_none());
    }
}
