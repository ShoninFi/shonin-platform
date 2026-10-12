//! Code identity: whether a vault is a clone of the audited implementation,
//! linked to the audited libraries, proven once per address and kept until
//! the branch it was proven on is replaced below it (contract code at an
//! address cannot change on one branch).
//!
//! The expected code comes from `contracts/evm/script/generate-view-code.py`:
//! each contract's runtime with its linked library addresses (and a
//! library's own embedded address) zeroed, its length and hash.

use std::collections::{BTreeMap, BTreeSet};

use alloy::eips::BlockId;
use alloy::primitives::{Address, B256, Bytes, keccak256};
use alloy::providers::{DynProvider, Provider};
use anyhow::{Context, Result, bail};
use serde::Deserialize;

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

fn identities() -> BTreeMap<Role, Identity> {
    serde_json::from_str(include_str!("code/identities.json"))
        .expect("the checked-in code identities are valid")
}

/// Code proven to be the audited code, by role and address.
#[derive(Clone, Debug, Default)]
pub struct CodeIdentities {
    verified: BTreeSet<(Role, Address)>,
    most: usize,
}

impl CodeIdentities {
    /// Keep at most `most` proven addresses (past that, proofs are not kept
    /// and are asked again).
    pub fn new(most: usize) -> Self {
        Self {
            verified: BTreeSet::new(),
            most,
        }
    }

    /// Forget every proof, after a replacement below where they were made.
    pub fn forget(&mut self) {
        self.verified.clear();
    }

    /// How many addresses are proven.
    pub fn len(&self) -> usize {
        self.verified.len()
    }

    pub fn is_empty(&self) -> bool {
        self.verified.is_empty()
    }

    fn keep(&mut self, role: Role, at: Address) {
        if self.verified.len() < self.most {
            self.verified.insert((role, at));
        }
    }

    async fn code(endpoint: &DynProvider, at: Address, block: BlockId) -> Result<Bytes> {
        endpoint
            .get_code_at(at)
            .block_id(block)
            .await
            .context("reading contract code")
    }

    /// Prove that `vault`, at `block` on `endpoint`, is a minimal clone of
    /// the audited implementation whose linked libraries are the audited
    /// libraries. Each address is read once and kept.
    pub async fn vault(
        &mut self,
        endpoint: &DynProvider,
        vault: Address,
        block: BlockId,
    ) -> Result<()> {
        let code = Self::code(endpoint, vault, block).await?;
        let implementation = clone_target(&code)?;
        if self.verified.contains(&(Role::Vault, implementation)) {
            return Ok(());
        }
        let identities = identities();
        let mut root = Self::code(endpoint, implementation, block).await?.to_vec();
        let children = normalized(
            implementation,
            Role::Vault,
            &mut root,
            &identities[&Role::Vault],
        )?;
        for &(role, at) in &children {
            if !self.verified.contains(&(role, at)) {
                let mut code = Self::code(endpoint, at, block).await?.to_vec();
                normalized(
                    at,
                    role,
                    &mut code,
                    identities.get(&role).context("unknown library identity")?,
                )?;
                self.keep(role, at);
            }
        }
        // The implementation only after every library it links was proven.
        self.keep(Role::Vault, implementation);
        Ok(())
    }
}

/// The implementation a minimal (EIP-1167) clone delegates to.
fn clone_target(code: &[u8]) -> Result<Address> {
    const PREFIX: [u8; 10] = [0x36, 0x3d, 0x3d, 0x37, 0x3d, 0x3d, 0x3d, 0x36, 0x3d, 0x73];
    const SUFFIX: [u8; 15] = [
        0x5a, 0xf4, 0x3d, 0x82, 0x80, 0x3e, 0x90, 0x3d, 0x91, 0x60, 0x2b, 0x57, 0xfd, 0x5b, 0xf3,
    ];
    if code.len() != 45 || code[..10] != PREFIX || code[30..] != SUFFIX {
        bail!("the vault is not a fixed implementation clone");
    }
    Ok(Address::from_slice(&code[10..30]))
}

/// Zero `code`'s masked spans after checking them, compare the rest with
/// `identity`, and return the libraries it links, by role.
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
        let end = mask
            .start
            .checked_add(mask.length)
            .context("code offset overflow")?;
        let value = code
            .get_mut(mask.start..end)
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
    if keccak256(&code) != identity.normalized_hash {
        bail!("the contract's code is not the audited code");
    }
    Ok(children)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checked_in_identities_name_the_vault_and_its_libraries() {
        let identities = identities();
        for role in [
            Role::Vault,
            Role::Upkeep,
            Role::Claims,
            Role::Posts,
            Role::Valuations,
        ] {
            assert!(identities.contains_key(&role), "{role:?}");
        }
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
            normalized(at, Role::Vault, &mut code, &identity).unwrap(),
            BTreeSet::from([(Role::Upkeep, child)])
        );
        let mut different_code = original.clone();
        different_code[0] = 1;
        assert!(normalized(at, Role::Vault, &mut different_code, &identity).is_err());
        let mut split_role = original;
        split_role[40..60].fill(8);
        assert!(normalized(at, Role::Vault, &mut split_role, &identity).is_err());
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
            normalized(at, Role::Claims, &mut code, &identity)
                .unwrap()
                .is_empty()
        );
        code[12..].fill(8);
        assert!(normalized(at, Role::Claims, &mut code, &identity).is_err());
        let invalid = Identity {
            length: 32,
            normalized_hash: keccak256([0; 32]),
            masks: vec![Mask {
                role: Role::Own,
                start: usize::MAX,
                length: 32,
            }],
        };
        assert!(normalized(at, Role::Claims, &mut code, &invalid).is_err());
    }

    #[test]
    fn only_a_fixed_minimal_clone_names_an_implementation() {
        let target = Address::repeat_byte(0x42);
        let mut code = alloy::hex::decode("363d3d373d3d3d363d73").unwrap();
        code.extend_from_slice(target.as_slice());
        code.extend(alloy::hex::decode("5af43d82803e903d91602b57fd5bf3").unwrap());
        assert_eq!(clone_target(&code).unwrap(), target);
        let mut longer = code.clone();
        longer.push(0);
        assert!(clone_target(&longer).is_err());
        code[0] = 0;
        assert!(clone_target(&code).is_err());
    }
}
