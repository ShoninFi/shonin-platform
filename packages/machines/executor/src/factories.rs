//! Every vault an EVM chain's deployment owes service to: those of the
//! deployment's vault factory, and of every factory it replaces.
//!
//! A replacement factory leaves the vaults made before it running, and their
//! agents are owed the same service as the newest ones. The walk is the one
//! the keeper makes for its upkeep: each factory names the one it replaces
//! (`previous`), followed back until there is none.

use std::collections::HashMap;

use alloy::primitives::Address;
use anyhow::Result;
use chain::Client;
use chain::bindings::factory::IVaultFactory;

alloy::sol! {
    /// The factory a vault factory replaces, or none. Not in the factory's
    /// shared interface.
    interface IReplacesFactory {
        function previous() external view returns (address);
    }
}

/// The most factories followed back from the deployment's, so a chain of
/// them that loops can't hold the sweep.
const MOST_FACTORIES: usize = 16;

/// What a chain's factories listed when last read, so an earlier factory
/// whose list can't be read this time still has its vaults served.
#[derive(Default)]
pub struct Factories {
    /// Each factory's vaults, and the factory it replaces, as last read.
    vaults: HashMap<Address, Vec<Address>>,
    previous: HashMap<Address, Address>,
    /// Whether an earlier factory's list couldn't be read, and that was
    /// said: said once while it lasts, and once more when it is over.
    unread_said: bool,
}

/// Every vault to serve: those of the deployment's factory, and of every
/// factory it replaces, followed back through `previous` until there is
/// none. A vault listed by two factories is served once.
///
/// The deployment's own factory must be read: its list failing is the
/// error. An earlier one whose list, or whose `previous`, can't be read
/// this time (a node's `eth_call` gas cap on a long list, say) is taken as
/// it was last read, so its vaults stay served; one never read yet ends the
/// walk there. That is said once at `warn` while it lasts. A factory whose
/// `previous` reverts (one made before factories named the one they
/// replace) names none.
pub async fn every_vault(
    chain: shared::Chain,
    client: &Client,
    factory: Address,
    factories: &mut Factories,
) -> Result<Vec<Address>> {
    let mut vaults = Vec::new();
    let mut unread = Vec::new();
    let mut seen: Vec<Address> = Vec::new();
    let mut at = Some(factory);
    while let Some(factory) = at.take() {
        seen.push(factory);
        let listed = match client.read(factory, IVaultFactory::vaultsCall {}).await {
            Ok(listed) => {
                factories.vaults.insert(factory, listed.clone());
                listed
            }
            Err(error) if seen.len() > 1 => {
                tracing::debug!(%chain, %factory, error = format!("{error:#}"), "an earlier factory's vaults not read; kept as last listed");
                unread.push(factory);
                factories.vaults.get(&factory).cloned().unwrap_or_default()
            }
            Err(error) => return Err(error),
        };
        for vault in listed {
            if !vaults.contains(&vault) {
                vaults.push(vault);
            }
        }
        let previous = match client
            .read(factory, IReplacesFactory::previousCall {})
            .await
        {
            Ok(previous) => {
                factories.previous.insert(factory, previous);
                previous
            }
            Err(error) if execution_reverted(&error) => Address::ZERO,
            Err(error) => {
                tracing::debug!(%chain, %factory, error = format!("{error:#}"), "could not read the factory this one replaces");
                if !unread.contains(&factory) {
                    unread.push(factory);
                }
                factories
                    .previous
                    .get(&factory)
                    .copied()
                    .unwrap_or(Address::ZERO)
            }
        };
        if previous != Address::ZERO && !seen.contains(&previous) && seen.len() < MOST_FACTORIES {
            at = Some(previous);
        }
    }
    if !unread.is_empty() {
        if !factories.unread_said {
            factories.unread_said = true;
            tracing::warn!(%chain, factories = ?unread, "could not read an earlier factory's vaults, or the factory it replaces; its vaults are served as last listed");
        }
    } else if std::mem::take(&mut factories.unread_said) {
        tracing::info!(%chain, "every factory's vaults are read again");
    }
    Ok(vaults)
}

/// Whether the node answered that the call itself reverts. Nodes say so
/// with code 3 or in those words. Only that is the contract's answer: any
/// other error the node gives is its own, and the call may go through once
/// that passes.
fn execution_reverted(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<alloy::transports::TransportError>()
            .and_then(|error| error.as_error_resp())
            .is_some_and(|refusal| {
                refusal.code == 3
                    || refusal
                        .message
                        .to_lowercase()
                        .contains("execution reverted")
            })
    })
}
