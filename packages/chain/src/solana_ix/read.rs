//! What a builder's inputs are worked out from, read from account bytes the
//! app fetched: the restricted list and the hash a depositor states for it,
//! the factory's terms and the hash an author states for them, and the
//! digests the programs bind to a cluster.

use alloy_primitives::keccak256;

use super::Pubkey;
use super::discriminator::{FACTORY_ACCOUNT, RESTRICTED_ACCOUNT};

/// keccak256 of `bytes`, as the programs hash.
pub fn keccak(bytes: &[u8]) -> [u8; 32] {
    keccak256(bytes).0
}

/// What stands for the cluster named `name` (`localnet`, `devnet`,
/// `mainnet`) in the programs' digests: `keccak256("solana:" + name)`.
pub fn cluster_value(name: &str) -> [u8; 32] {
    keccak(&[b"solana:".as_slice(), name.as_bytes()].concat())
}

/// A purpose as a digest binds it: its keccak256.
pub fn purpose(name: &str) -> [u8; 32] {
    keccak(name.as_bytes())
}

/// The hash a depositor's statement names: keccak256 of the restricted
/// list's Borsh encoding (its length as a 32-bit little-endian number, then
/// each 32-byte code). The vault takes a deposit stating it only while it is
/// the hash of the vault's list, so the app hashes the list it showed.
pub fn restricted_hash(codes: &[[u8; 32]]) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(4 + 32 * codes.len());
    bytes.extend_from_slice(&(codes.len() as u32).to_le_bytes());
    for code in codes {
        bytes.extend_from_slice(code);
    }
    keccak(&bytes)
}

/// A restricted code as a person reads it (`US`, `SANCTIONED`): the text
/// before its zero padding.
pub fn code_text(code: &[u8; 32]) -> String {
    let end = code.iter().position(|b| *b == 0).unwrap_or(32);
    String::from_utf8_lossy(&code[..end]).into_owned()
}

/// The codes of a vault's restricted list, from the bytes of its account at
/// [`super::address::restricted`]: `None` if they are not a restricted list.
pub fn restricted_codes(data: &[u8]) -> Option<Vec<[u8; 32]>> {
    // The discriminator, the vault, the bump, then the codes.
    if data.get(..8)? != RESTRICTED_ACCOUNT {
        return None;
    }
    let len = u32::from_le_bytes(data.get(41..45)?.try_into().ok()?) as usize;
    let body = data.get(45..len.checked_mul(32)?.checked_add(45)?)?;
    Some(body.as_chunks::<32>().0.to_vec())
}

/// How long the factory's terms are in Borsh: four fees, the exit band, the
/// minimum deposit, the minimum tank, the dead assets and ten timers.
const TERMS_LEN: usize = 4 * 2 + 2 + 3 * 8 + 10 * 8;
/// Where the terms start in the factory's account: after the discriminator
/// and eleven 32-byte fields.
const TERMS_AT: usize = 8 + 11 * 32;

/// What an author's activation is built from in the factory's settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Factory {
    pub admin: Pubkey,
    /// Where fees and bills go.
    pub platform: Pubkey,
    /// The dollar.
    pub mint: Pubkey,
    pub token_program: Pubkey,
    /// What stands for this cluster in digests.
    pub cluster: [u8; 32],
    /// The vault program this one replaces, or the default key.
    pub previous: Pubkey,
    /// The terms' Borsh encoding, which [`Factory::terms_hash`] hashes.
    pub terms: [u8; TERMS_LEN],
    pub minimum_deposit: u64,
    pub minimum_tank: u64,
}

impl Factory {
    /// The factory's settings from the bytes of its account at
    /// [`super::address::factory`]: `None` if they are not the factory's.
    pub fn read(data: &[u8]) -> Option<Self> {
        if data.get(..8)? != FACTORY_ACCOUNT {
            return None;
        }
        let key = |at: usize| -> Option<Pubkey> {
            Some(Pubkey::new_from_array(
                data.get(at..at + 32)?.try_into().ok()?,
            ))
        };
        let terms: [u8; TERMS_LEN] = data.get(TERMS_AT..TERMS_AT + TERMS_LEN)?.try_into().ok()?;
        let u64_at = |at: usize| u64::from_le_bytes(terms[at..at + 8].try_into().expect("8"));
        Some(Self {
            admin: key(8)?,
            platform: key(40)?,
            mint: key(72)?,
            token_program: key(104)?,
            cluster: data.get(136..168)?.try_into().ok()?,
            previous: key(328)?,
            minimum_deposit: u64_at(10),
            minimum_tank: u64_at(18),
            terms,
        })
    }

    /// The hash an author's activation states for the terms they read:
    /// keccak256 of the terms' Borsh encoding.
    pub fn terms_hash(&self) -> [u8; 32] {
        keccak(&self.terms)
    }
}

/// What a vault's executor signs for a running usage total: keccak256 of the
/// Borsh encoding of the purpose (`keccak256("shonin-usage")`), the cluster
/// value, the vault program, the vault, the total and when it was signed.
pub fn usage_digest(cluster: &[u8; 32], vault: &Pubkey, total: u64, as_of: u64) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(4 * 32 + 16);
    bytes.extend_from_slice(&purpose("shonin-usage"));
    bytes.extend_from_slice(cluster);
    bytes.extend_from_slice(super::ids::VAULT.as_ref());
    bytes.extend_from_slice(vault.as_ref());
    bytes.extend_from_slice(&total.to_le_bytes());
    bytes.extend_from_slice(&as_of.to_le_bytes());
    keccak(&bytes)
}
