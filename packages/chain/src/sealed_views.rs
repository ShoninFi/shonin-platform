//! Bounded, hash-pinned observations of audited vault storage getters.
//! Ordinary clients, simulations and sends never use these observations.

use crate::{
    Client,
    bindings::{station::IStationVault, vault::IVault},
};
use alloy::eips::BlockId;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::providers::{DynProvider, Provider};
use alloy::rpc::types::{TransactionRequest, state::AccountOverride};
use alloy_sol_types::SolCall;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

alloy_sol_types::sol!("../../contracts/evm/src/ViewReads.sol");
pub use ViewReads::{Read as ViewRead, Result as ViewResult, readCall};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    Vault,
    Upkeep,
    Claims,
    Posts,
    Valuations,
    #[serde(rename = "self")]
    Own,
}
#[derive(Deserialize)]
struct Mask {
    role: Role,
    start: usize,
    length: usize,
}
#[derive(Deserialize)]
struct Identity {
    length: usize,
    normalized_hash: B256,
    masks: Vec<Mask>,
}

/// Positive code proofs shared only within one indexer refresh.
#[derive(Default)]
pub struct Proofs {
    block: Option<BlockId>,
    endpoint: Option<String>,
    pin_checked: bool,
    verified: BTreeSet<(Role, Address)>,
}

impl Proofs {
    fn begin(&mut self, client: &Client, block: BlockId) {
        if self.block != Some(block) || self.endpoint.as_deref() != Some(client.rpc_url.as_str()) {
            self.verified.clear();
            self.pin_checked = false;
            self.block = Some(block);
            self.endpoint = Some(client.rpc_url.clone());
        }
    }

    async fn code(endpoint: &DynProvider, at: Address, block: BlockId) -> Result<Bytes> {
        endpoint
            .get_code_at(at)
            .block_id(block)
            .await
            .context("the observed contract code")
    }

    fn normalized(
        at: Address,
        role: Role,
        code: &mut [u8],
        identity: &Identity,
    ) -> Result<BTreeSet<(Role, Address)>> {
        if code.len() != identity.length {
            bail!("the contract has another code length");
        }
        let mut children = BTreeSet::new();
        let mut linked = BTreeMap::new();
        for mask in &identity.masks {
            let value = code
                .get_mut(
                    mask.start
                        ..mask
                            .start
                            .checked_add(mask.length)
                            .context("code offset overflow")?,
                )
                .context("a code identity mask is out of bounds")?;
            if mask.role == Role::Own {
                if mask.length != 32 || value[..12] != [0; 12] || value[12..] != at.as_slice()[..] {
                    bail!("the library's embedded identity does not match its address");
                }
            } else {
                if role != Role::Vault || mask.length != 20 {
                    bail!("unsupported linked code");
                }
                let address = Address::from_slice(value);
                if let Some(previous) = linked.insert(mask.role, address)
                    && previous != address
                {
                    bail!("one library role names different addresses");
                }
                children.insert((mask.role, address));
            }
            value.fill(0);
        }
        if keccak256(code) != identity.normalized_hash {
            bail!("the contract's code is not an audited storage reader");
        }
        Ok(children)
    }

    async fn vault(
        &mut self,
        client: &Client,
        endpoint: &DynProvider,
        vault: Address,
        block: BlockId,
    ) -> Result<()> {
        self.begin(client, block);
        let code = Self::code(endpoint, vault, block).await?;
        let prefix = alloy::hex::decode("363d3d373d3d3d363d73")?;
        let suffix = alloy::hex::decode("5af43d82803e903d91602b57fd5bf3")?;
        if code.len() != 45 || code[..10] != prefix || code[30..] != suffix {
            bail!("the vault is not a fixed implementation clone");
        }
        let implementation = Address::from_slice(&code[10..30]);
        if self.verified.contains(&(Role::Vault, implementation)) {
            return Ok(());
        }
        let identities: BTreeMap<Role, Identity> =
            serde_json::from_str(include_str!("sealed_views/identities.json"))?;
        let mut root = Self::code(endpoint, implementation, block).await?.to_vec();
        let children = Self::normalized(
            implementation,
            Role::Vault,
            &mut root,
            &identities[&Role::Vault],
        )?;
        for &(role, at) in &children {
            if !self.verified.contains(&(role, at)) {
                let mut code = Self::code(endpoint, at, block).await?.to_vec();
                Self::normalized(
                    at,
                    role,
                    &mut code,
                    identities.get(&role).context("unknown library identity")?,
                )?;
                if self.verified.len() < client.rpc_policy().log_batch_addresses as usize {
                    self.verified.insert((role, at));
                }
            }
        }
        // Record the root only after every linked mutating library was proved.
        if self.verified.len() < client.rpc_policy().log_batch_addresses as usize {
            self.verified.insert((Role::Vault, implementation));
        }
        Ok(())
    }
}

fn missing_hash(code: i64, message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    (code == -32001 && message == "resource not found")
        || matches!(code, -32000 | -32001 | -32602)
            && [
                "block not found",
                "header not found",
                "unknown block",
                "unknown header",
                "cannot find block",
            ]
            .iter()
            .any(|text| message.contains(text))
}

/// Only this constructor's fixed no-argument getter list can be observed.
#[derive(Default)]
pub struct SealedViews {
    vault: Option<Address>,
    values: BTreeMap<[u8; 4], Bytes>,
}

impl SealedViews {
    pub async fn observe(
        client: &Client,
        vault: Address,
        observed: (u64, u64, B256),
        proofs: &mut Proofs,
    ) -> Result<Self> {
        Self::observe_selected(client, vault, observed, proofs, false).await
    }

    /// A sealed storage observation used only to decide whether a keeper
    /// should perform its ordinary fresh round. No entry authorizes a send.
    pub async fn observe_quiet(
        client: &Client,
        vault: Address,
        observed: (u64, u64, B256),
        proofs: &mut Proofs,
    ) -> Result<Self> {
        Self::observe_selected(client, vault, observed, proofs, true).await
    }

    async fn observe_selected(
        client: &Client,
        vault: Address,
        observed: (u64, u64, B256),
        proofs: &mut Proofs,
        quiet: bool,
    ) -> Result<Self> {
        let (number, time, hash) = observed;
        let block = BlockId::hash_canonical(hash);
        // Capability, code and execution must use the same dedicated endpoint.
        // A failing or unsupported first endpoint leaves ordinary live reads
        // to their existing failover; no proof authorizes another endpoint.
        let endpoint = client
            .each_endpoint()
            .into_iter()
            .next()
            .context("no dedicated read endpoint")?;
        proofs.begin(client, block);
        let policy = client.rpc_policy();
        let mut calls = Vec::new();
        macro_rules! getters { ($($call:ident),+ $(,)?) => { $(calls.push(Bytes::from(IVault::$call {}.abi_encode()));)+ }; }
        if quiet {
            // These getters and their linked library paths inspect only the
            // verified implementation's storage. They cannot call a venue.
            getters!(
                stateCall,
                timersCall,
                tankCall,
                homeStationCall,
                lastPricedCall,
                lastPaymentCall,
                nextUnpricedCall,
                claimCountCall,
                unitsCall,
                needsValuationFirstCall
            );
            calls.push(Bytes::from(IStationVault::valuingCall {}.abi_encode()));
            calls.push(Bytes::from(IStationVault::unsettledCall {}.abi_encode()));
        } else {
            getters!(
                strategyIdCall,
                authorCall,
                createdAtCall,
                stateCall,
                bundleHashCall,
                feesCall,
                timersCall,
                tankCall,
                allowancesCall,
                cashCall,
                escrowCall,
                markCall,
                lastPaymentCall,
                idleFloorCall,
                totalSupplyCall,
                homeStationCall,
                exitBandBpsCall,
                sharePriceCall,
                totalValueCall,
                restrictedCall
            );
        }
        calls.push(Bytes::from(IStationVault::haltedAtCall {}.abi_encode()));
        if calls.len() > policy.view_batch_reads as usize {
            bail!("the storage observation exceeds its item bound");
        }
        let call = readCall {
            reads: calls
                .iter()
                .map(|data| ViewRead {
                    target: vault,
                    data: data.clone(),
                })
                .collect(),
            innerGas: U256::from(policy.view_inner_gas),
            resultBytes: U256::from(policy.view_result_bytes),
            inputBytes: U256::from(policy.view_batch_input_bytes),
        };
        let input = call.abi_encode();
        if input.len() > policy.view_batch_input_bytes as usize {
            bail!("the storage observation exceeds its input bound");
        }
        let parent_number = number
            .checked_sub(1)
            .context("the genesis block has no parent")?;
        let parent = endpoint
            .get_block(BlockId::number(parent_number))
            .await?
            .context("the observation's parent block is missing")?;
        if parent.header.number != parent_number {
            bail!("the node returned another parent number");
        }
        let request = TransactionRequest::default()
            .with_from(Address::ZERO)
            .with_to(Address::ZERO)
            .with_input(input)
            .with_gas_limit(policy.view_outer_gas);
        let runtime = Bytes::from(alloy::hex::decode(
            include_str!("sealed_views/ViewReads.hex").trim(),
        )?);
        if !proofs.pin_checked {
            let probe = readCall {
                reads: Vec::new(),
                innerGas: call.innerGas,
                resultBytes: call.resultBytes,
                inputBytes: call.inputBytes,
            };
            let probe_request = request.clone().with_input(probe.abi_encode());
            let historical = endpoint
                .call(probe_request.clone())
                .block(BlockId::hash_canonical(parent.header.hash))
                .account_override(
                    Address::ZERO,
                    AccountOverride::default().with_code(runtime.clone()),
                )
                .await?;
            if historical.len() > 256 {
                bail!("the hash-pin probe returned an oversized answer");
            }
            let historical = readCall::abi_decode_returns(&historical)?;
            if historical.number != U256::from(parent_number)
                || historical.timestamp != U256::from(parent.header.timestamp)
                || historical.parentHash != parent.header.parent_hash
                || !historical.results.is_empty()
            {
                bail!("the node does not execute the helper at its historical hash pin");
            }
            let mut unknown = hash;
            unknown[0] ^= 0x80;
            match endpoint
                .call(probe_request)
                .block(BlockId::hash_canonical(unknown))
                .account_override(
                    Address::ZERO,
                    AccountOverride::default().with_code(runtime.clone()),
                )
                .await
            {
                Err(alloy::rpc::json_rpc::RpcError::ErrorResp(refusal))
                    if missing_hash(refusal.code, &refusal.message) => {}
                _ => bail!("the node has not proved rejection of an unknown hash pin"),
            }
            proofs.pin_checked = true;
        }
        proofs.vault(client, &endpoint, vault, block).await?;
        let returned = endpoint
            .call(request)
            .block(block)
            .account_override(Address::ZERO, AccountOverride::default().with_code(runtime))
            .await?;
        if returned.len() > calls.len() * (policy.view_result_bytes as usize + 160) + 256 {
            bail!("the storage observation exceeds its output bound");
        }
        let result = readCall::abi_decode_returns(&returned)?;
        if result.number != U256::from(number)
            || result.timestamp != U256::from(time)
            || result.parentHash != parent.header.hash
            || result.results.len() != calls.len()
        {
            bail!("the storage observation did not run at its observed header");
        }
        let mut values = BTreeMap::new();
        for (call, result) in calls.into_iter().zip(result.results) {
            if result.success && result.data.len() <= policy.view_result_bytes as usize {
                values.insert(
                    call[..4].try_into().expect("fixed getter selector"),
                    result.data,
                );
            }
        }
        Ok(Self {
            vault: Some(vault),
            values,
        })
    }

    /// Unsupported, failed, oversized or malformed results use an ordinary
    /// live read. No caller receives an older observation as a fallback.
    pub async fn read<C: SolCall>(
        &self,
        client: &Client,
        vault: Address,
        call: C,
    ) -> Result<C::Return> {
        if let Some(value) = self.observed(vault, &call) {
            return Ok(value);
        }
        client.read(vault, call).await
    }

    /// Only a successfully decoded value from this exact observation. A
    /// missing entry cannot silently become a live or older fallback when
    /// deciding whether work may be deferred.
    pub fn observed<C: SolCall>(&self, vault: Address, call: &C) -> Option<C::Return> {
        if self.vault != Some(vault) || call.abi_encode().len() != 4 {
            return None;
        }
        C::abi_decode_returns(self.values.get(&C::SELECTOR)?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_hash_requires_a_block_or_resource_not_found_refusal() {
        assert!(missing_hash(-32001, "Resource not found"));
        assert!(missing_hash(-32000, "header not found"));
        assert!(!missing_hash(-32601, "Resource not found"));
        assert!(!missing_hash(-32000, "rate limited"));
        assert!(!missing_hash(-32602, "unsupported argument"));
        assert!(!missing_hash(-32000, "execution reverted"));
    }

    #[test]
    fn address_masks_preserve_code_identity_and_bind_each_library_role() {
        let at = Address::repeat_byte(7);
        let child = Address::repeat_byte(3);
        let mut code = vec![0; 80];
        code[10..30].copy_from_slice(child.as_slice());
        code[40..60].copy_from_slice(child.as_slice());
        let identity = Identity {
            length: 80,
            normalized_hash: keccak256([0; 80]),
            masks: vec![
                Mask {
                    role: Role::Upkeep,
                    start: 10,
                    length: 20,
                },
                Mask {
                    role: Role::Upkeep,
                    start: 40,
                    length: 20,
                },
            ],
        };
        let original = code.clone();
        assert_eq!(
            Proofs::normalized(at, Role::Vault, &mut code, &identity).unwrap(),
            BTreeSet::from([(Role::Upkeep, child)])
        );
        let mut different_code = original.clone();
        different_code[0] = 1;
        assert!(Proofs::normalized(at, Role::Vault, &mut different_code, &identity).is_err());
        let mut split_role = original;
        split_role[40..60].fill(8);
        assert!(Proofs::normalized(at, Role::Vault, &mut split_role, &identity).is_err());
    }

    #[test]
    fn library_self_identity_and_mask_bounds_fail_closed() {
        let at = Address::repeat_byte(7);
        let identity = Identity {
            length: 32,
            normalized_hash: keccak256([0; 32]),
            masks: vec![Mask {
                role: Role::Own,
                start: 0,
                length: 32,
            }],
        };
        let mut code = vec![0; 32];
        code[12..].copy_from_slice(at.as_slice());
        assert!(
            Proofs::normalized(at, Role::Claims, &mut code, &identity)
                .unwrap()
                .is_empty()
        );
        code[12..].fill(8);
        assert!(Proofs::normalized(at, Role::Claims, &mut code, &identity).is_err());
        let invalid = Identity {
            length: 32,
            normalized_hash: keccak256([0; 32]),
            masks: vec![Mask {
                role: Role::Own,
                start: usize::MAX,
                length: 32,
            }],
        };
        assert!(Proofs::normalized(at, Role::Claims, &mut code, &invalid).is_err());
    }

    #[test]
    fn generated_storage_reader_inputs_match_the_current_contract_sources() {
        let sources: BTreeMap<String, B256> =
            serde_json::from_str(include_str!("sealed_views/sources.json")).unwrap();
        let evm = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm");
        assert!(sources.contains_key("src/ViewReads.sol"));
        for (path, expected) in sources {
            let code = std::fs::read(evm.join(&path)).unwrap();
            assert_eq!(
                keccak256(code),
                expected,
                "regenerate the storage reader identities after rebuilding {path}"
            );
        }
        let runtime =
            alloy::hex::decode(include_str!("sealed_views/ViewReads.hex").trim()).unwrap();
        let expected: B256 = include_str!("sealed_views/ViewReads.hash")
            .trim()
            .parse()
            .unwrap();
        assert_eq!(keccak256(runtime), expected);
    }
}
