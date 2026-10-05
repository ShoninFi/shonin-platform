//! Solana keys: from a mnemonic, the way `solana-keygen` and wallets derive
//! them (the BIP-39 seed, then SLIP-0010 for ed25519 along
//! `m/44'/501'/<index>'/0'`), and from the keypair files the Solana tools
//! write (the 32-byte secret then the 32-byte public key, as a JSON array of
//! numbers).
//!
//! The platform's roles on a cluster are keys of one mnemonic: on a local
//! cluster the well-known development phrase, on devnet the team's
//! (`SHONIN_TESTNET_SOLANA_MNEMONIC`), and on mainnet none, where clients
//! only read.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha512};

use super::{Keypair, Pubkey, Signer};
use crate::client::Role;
use crate::config::NetworkName;

/// The mnemonic the testnet Solana accounts are derived from.
pub const TESTNET_SOLANA_MNEMONIC: &str = "SHONIN_TESTNET_SOLANA_MNEMONIC";

/// The phrase every development chain publishes its accounts under. Nothing
/// it derives is a secret, which is what makes a chain a development one.
pub const DEVELOPMENT_MNEMONIC: &str =
    "test test test test test test test test test test test junk";

/// Which key of the mnemonic plays which part. The deployer pays for and
/// sets up every program; the program's own key fixes the registry's address
/// on devnet; each role is at [`ROLE_BASE`] plus its index.
pub const DEPLOYER: u32 = 0;
pub const PROGRAM: u32 = 1;
pub const ROLE_BASE: u32 = 10;

/// The keypair at `m/44'/501'/<index>'/0'` of a mnemonic.
pub fn keypair_from_mnemonic(phrase: &str, index: u32) -> Result<Keypair> {
    use alloy::signers::local::coins_bip39::{English, Mnemonic};
    // The library's own error quotes the phrase; it is never passed on.
    let mnemonic = Mnemonic::<English>::new_from_phrase(phrase.trim())
        .map_err(|_| anyhow!("the Solana mnemonic is not a valid BIP-39 phrase"))?;
    let seed = mnemonic
        .to_seed(None)
        .map_err(|_| anyhow!("the Solana mnemonic gives no seed"))?;
    Ok(Keypair::new_from_array(derive(&seed, &[44, 501, index, 0])))
}

/// The testnet mnemonic, from the environment.
pub fn testnet_mnemonic() -> Result<String> {
    std::env::var(TESTNET_SOLANA_MNEMONIC)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "the testnet Solana accounts come from {TESTNET_SOLANA_MNEMONIC}; set it in .env"
            )
        })
}

/// The mnemonic a network's role keys come from, or none on mainnet.
pub fn mnemonic_for(network: NetworkName) -> Result<Option<String>> {
    match network {
        NetworkName::Local => Ok(Some(DEVELOPMENT_MNEMONIC.to_string())),
        NetworkName::Testnet => testnet_mnemonic().map(Some),
        NetworkName::Mainnet => Ok(None),
    }
}

/// The platform's role keys on one cluster.
pub struct RoleKeys {
    pub deployer: Keypair,
    roles: Vec<Keypair>,
}

impl RoleKeys {
    pub fn from_mnemonic(phrase: &str) -> Result<Self> {
        let deployer = keypair_from_mnemonic(phrase, DEPLOYER)?;
        let roles = (0..crate::client::ROLE_COUNT as u32)
            .map(|index| keypair_from_mnemonic(phrase, ROLE_BASE + index))
            .collect::<Result<_>>()?;
        Ok(Self { deployer, roles })
    }

    pub fn get(&self, role: Role) -> &Keypair {
        &self.roles[role.account_index()]
    }

    pub fn address(&self, role: Role) -> Pubkey {
        self.get(role).pubkey()
    }

    /// The key at `address`, the deployer's or a role's, if it is one of
    /// these.
    pub fn find(&self, address: &Pubkey) -> Option<&Keypair> {
        std::iter::once(&self.deployer)
            .chain(&self.roles)
            .find(|key| key.pubkey() == *address)
    }
}

/// Read a keypair file the Solana tools wrote.
pub fn read_keypair(path: &Path) -> Result<Keypair> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let bytes: Vec<u8> = text
        .trim()
        .trim_matches(['[', ']'])
        .split(',')
        .map(|n| n.trim().parse::<u8>())
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("{} is not a keypair file", path.display()))?;
    if bytes.len() != 64 {
        bail!(
            "{} holds {} bytes, not a keypair's 64",
            path.display(),
            bytes.len()
        );
    }
    let secret: [u8; 32] = bytes[..32].try_into().expect("32 bytes");
    let pair = Keypair::new_from_array(secret);
    if pair.pubkey().to_bytes()[..] != bytes[32..] {
        bail!("{}'s public key is not its secret's", path.display());
    }
    Ok(pair)
}

/// Write a keypair file the Solana tools read, readable only by its owner.
pub fn write_keypair(pair: &Keypair, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes: Vec<String> = pair.to_bytes().iter().map(u8::to_string).collect();
    std::fs::write(path, format!("[{}]", bytes.join(",")))
        .with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// SLIP-0010 for ed25519: every step is hardened.
fn derive(seed: &[u8], path: &[u32]) -> [u8; 32] {
    let master = hmac_sha512(b"ed25519 seed", seed);
    let (mut key, mut chain) = split(&master);
    for index in path {
        let mut data = Vec::with_capacity(37);
        data.push(0);
        data.extend_from_slice(&key);
        data.extend_from_slice(&(index | 0x8000_0000).to_be_bytes());
        let child = hmac_sha512(&chain, &data);
        (key, chain) = split(&child);
    }
    key
}

fn split(bytes: &[u8; 64]) -> ([u8; 32], [u8; 32]) {
    let mut left = [0u8; 32];
    let mut right = [0u8; 32];
    left.copy_from_slice(&bytes[..32]);
    right.copy_from_slice(&bytes[32..]);
    (left, right)
}

/// HMAC (RFC 2104) over SHA-512, whose block is 128 bytes.
fn hmac_sha512(key: &[u8], message: &[u8]) -> [u8; 64] {
    let mut block = [0u8; 128];
    if key.len() > block.len() {
        block[..64].copy_from_slice(&Sha512::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.map(|b| b ^ byte);

    let mut inner = Sha512::new();
    inner.update(pad(0x36));
    inner.update(message);
    let inner = inner.finalize();

    let mut outer = Sha512::new();
    outer.update(pad(0x5c));
    outer.update(inner);
    let mut out = [0u8; 64];
    out.copy_from_slice(&outer.finalize());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// SLIP-0010, ed25519 test vector 1.
    #[test]
    fn derivation_matches_the_published_vectors() {
        let seed: Vec<u8> = (0u8..16).collect();
        assert_eq!(
            hex(&derive(&seed, &[])),
            "2b4be7f19ee27bbf30c667b642d5f4aa69fd169872f8fc3059c08ebae2eb19e7"
        );
        assert_eq!(
            hex(&derive(&seed, &[0])),
            "68e0fe46dfb67e368c75379acec591dad19df3cde26e63b93a8e704f1dade7a3"
        );
        assert_eq!(
            hex(&derive(&seed, &[0, 1])),
            "b1d0bad404bf35da785a64ca1ac54b2617211d2777696fbffaf208f746ae84f2"
        );
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex(&hmac_sha512(b"Jefe", b"what do ya want for nothing?")),
            "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea250554\
             9758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737"
        );
    }

    #[test]
    fn a_keypair_file_reads_back_as_the_same_key() {
        let pair = keypair_from_mnemonic(DEVELOPMENT_MNEMONIC, 0).unwrap();
        let dir = std::env::temp_dir().join(format!("shonin-solana-keys-{}", std::process::id()));
        let path = dir.join("key.json");
        write_keypair(&pair, &path).unwrap();
        assert_eq!(read_keypair(&path).unwrap().pubkey(), pair.pubkey());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_role_has_a_key_of_its_own() {
        let keys = RoleKeys::from_mnemonic(DEVELOPMENT_MNEMONIC).unwrap();
        let mut seen: Vec<Pubkey> = [
            Role::Admin,
            Role::Platform,
            Role::Attestor,
            Role::Builder,
            Role::Reviewer,
            Role::Executor,
            Role::Keeper,
            Role::Operator,
            Role::Author,
            Role::Viewer,
        ]
        .into_iter()
        .map(|role| keys.address(role))
        .collect();
        seen.push(keys.deployer.pubkey());
        let count = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), count);
    }
}
