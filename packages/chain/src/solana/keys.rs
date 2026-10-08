//! Solana keys: from a mnemonic, the way `solana-keygen` and wallets derive
//! them (the BIP-39 seed, then SLIP-0010 for ed25519 along
//! `m/44'/501'/<index>'/0'`), and from the keypair files the Solana tools
//! write (the 32-byte secret then the 32-byte public key, as a JSON array of
//! numbers).
//!
//! The platform's roles on a cluster are keys of one mnemonic: on a local
//! cluster the well-known development phrase, on devnet the team's
//! (`SHONIN_TESTNET_SOLANA_MNEMONIC`), and on mainnet none, where clients
//! only read. A devnet client without the mnemonic (a service in the cloud)
//! only reads, or signs as the keeper alone with the keeper's own key
//! ([`TESTNET_KEEPER_SOLANA_KEY`]), and never holds the mnemonic.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha512};

use super::{Keypair, Pubkey, Signer};
use crate::client::Role;
use crate::config::NetworkName;

/// The mnemonic the testnet Solana accounts are derived from.
pub const TESTNET_SOLANA_MNEMONIC: &str = "SHONIN_TESTNET_SOLANA_MNEMONIC";

/// The keeper's own devnet key, for a keeper that runs without the mnemonic:
/// it signs as the keeper and nothing else. Its keypair as the Solana tools
/// write one (a JSON array of its 64 bytes) or as wallets export one (the
/// same 64 bytes in base58).
pub const TESTNET_KEEPER_SOLANA_KEY: &str = "SHONIN_TESTNET_KEEPER_SOLANA_KEY";

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

/// Who signs on a Solana cluster.
pub enum Signing {
    /// Every role, from a mnemonic: the development one locally, the team's
    /// on devnet.
    Roles(Arc<RoleKeys>),
    /// The keeper alone, from its own key: a devnet service without the
    /// mnemonic. Checked against the keeper the deployment names before it
    /// signs anything ([`Signing::check_keeper`]).
    Keeper(Box<Keypair>),
    /// Nobody: mainnet, or devnet with neither the mnemonic nor the keeper's
    /// key.
    ReadOnly,
}

impl Signing {
    /// Who signs on `network`, from the environment: the development
    /// mnemonic locally; on a testnet the team's mnemonic when it is set,
    /// otherwise the keeper's own key when that is set, otherwise nobody;
    /// nobody on mainnet.
    pub fn for_network(network: NetworkName) -> Result<Self> {
        let set = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        Self::from(
            network,
            set(TESTNET_SOLANA_MNEMONIC).as_deref(),
            set(TESTNET_KEEPER_SOLANA_KEY).as_deref(),
        )
    }

    /// [`Signing::for_network`], given what the environment holds. A testnet
    /// mnemonic that is the published development phrase is refused: every
    /// key it derives is anyone's, so a public cluster's roles must never
    /// come from it.
    pub fn from(
        network: NetworkName,
        mnemonic: Option<&str>,
        keeper_key: Option<&str>,
    ) -> Result<Self> {
        match network {
            NetworkName::Local => Ok(Self::Roles(Arc::new(RoleKeys::from_mnemonic(
                DEVELOPMENT_MNEMONIC,
            )?))),
            NetworkName::Testnet => {
                if let Some(phrase) = mnemonic {
                    let keys = RoleKeys::from_mnemonic(phrase)?;
                    if is_development(&keys) {
                        bail!(
                            "{TESTNET_SOLANA_MNEMONIC} is the published development phrase, whose keys are anyone's: a public cluster's roles come from the team's own"
                        );
                    }
                    return Ok(Self::Roles(Arc::new(keys)));
                }
                match keeper_key {
                    Some(text) => keypair_from_text(text)
                        .map(|key| Self::Keeper(Box::new(key)))
                        .map_err(|_| anyhow!("{TESTNET_KEEPER_SOLANA_KEY} is not a keypair")),
                    None => Ok(Self::ReadOnly),
                }
            }
            NetworkName::Mainnet => Ok(Self::ReadOnly),
        }
    }

    /// The key that signs as `role`, if this holds it.
    pub fn get(&self, role: Role) -> Option<&Keypair> {
        match self {
            Self::Roles(keys) => Some(keys.get(role)),
            Self::Keeper(key) if role == Role::Keeper => Some(&**key),
            Self::Keeper(_) | Self::ReadOnly => None,
        }
    }

    /// The key at `address`, the deployer's or a role's, if this holds it.
    pub fn find(&self, address: &Pubkey) -> Option<&Keypair> {
        match self {
            Self::Roles(keys) => keys.find(address),
            Self::Keeper(key) => (key.pubkey() == *address).then_some(&**key),
            Self::ReadOnly => None,
        }
    }

    /// A keeper's own key is refused unless it is the keeper `named` (the
    /// deployment's, base58): signing as anyone else's keeper is never right.
    /// The key itself never appears in the error.
    pub fn check_keeper(&self, named: &str) -> Result<()> {
        let Self::Keeper(key) = self else {
            return Ok(());
        };
        if key.pubkey().to_string() != named.trim() {
            bail!(
                "{TESTNET_KEEPER_SOLANA_KEY} is the key of {}, and the deployment's keeper is {named}: refused",
                key.pubkey()
            );
        }
        Ok(())
    }
}

/// Whether `keys` are the development mnemonic's.
fn is_development(keys: &RoleKeys) -> bool {
    keypair_from_mnemonic(DEVELOPMENT_MNEMONIC, DEPLOYER)
        .is_ok_and(|development| development.pubkey() == keys.deployer.pubkey())
}

/// A keypair as text: a JSON array of its 64 bytes, as the Solana tools
/// write one, or the same 64 bytes in base58, as wallets export one. The
/// text is never quoted back in an error.
pub fn keypair_from_text(text: &str) -> Result<Keypair> {
    let text = text.trim();
    let bytes: Vec<u8> = if text.starts_with('[') {
        keypair_bytes(text).ok_or_else(|| anyhow!("not a keypair's JSON array"))?
    } else {
        bs58::decode(text)
            .into_vec()
            .map_err(|_| anyhow!("neither a keypair's JSON array nor base58"))?
    };
    keypair_of(&bytes)
}

/// The numbers of a keypair file's JSON array.
fn keypair_bytes(text: &str) -> Option<Vec<u8>> {
    text.trim()
        .strip_prefix('[')?
        .strip_suffix(']')?
        .split(',')
        .map(|n| n.trim().parse::<u8>().ok())
        .collect()
}

/// The keypair of 64 bytes, its secret then its public key, checked to be
/// one.
fn keypair_of(bytes: &[u8]) -> Result<Keypair> {
    if bytes.len() != 64 {
        bail!("{} bytes, not a keypair's 64", bytes.len());
    }
    let secret: [u8; 32] = bytes[..32].try_into().expect("32 bytes");
    let pair = Keypair::new_from_array(secret);
    if pair.pubkey().to_bytes()[..] != bytes[32..] {
        bail!("the public key is not the secret's");
    }
    Ok(pair)
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
    let bytes =
        keypair_bytes(&text).ok_or_else(|| anyhow!("{} is not a keypair file", path.display()))?;
    keypair_of(&bytes).with_context(|| format!("{} is not a keypair", path.display()))
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

    /// A valid phrase that is not the development one (BIP-39's own test
    /// vector).
    const ANOTHER_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn keeper_of(phrase: &str) -> Keypair {
        RoleKeys::from_mnemonic(phrase)
            .unwrap()
            .get(Role::Keeper)
            .insecure_clone()
    }

    fn json_of(pair: &Keypair) -> String {
        let bytes: Vec<String> = pair.to_bytes().iter().map(u8::to_string).collect();
        format!("[{}]", bytes.join(","))
    }

    #[test]
    fn a_testnet_without_the_mnemonic_signs_as_the_keeper_alone_with_its_own_key() {
        let keeper = keeper_of(ANOTHER_MNEMONIC);
        let as_base58 = bs58::encode(keeper.to_bytes()).into_string();
        for text in [
            json_of(&keeper),
            as_base58,
            format!("  {}\n", json_of(&keeper)),
        ] {
            let signing = Signing::from(NetworkName::Testnet, None, Some(&text)).unwrap();
            assert_eq!(
                signing.get(Role::Keeper).map(Signer::pubkey),
                Some(keeper.pubkey())
            );
            for role in [Role::Admin, Role::Executor, Role::Attestor, Role::Operator] {
                assert!(signing.get(role).is_none(), "{role:?} has no key here");
            }
            assert!(signing.find(&keeper.pubkey()).is_some());
            assert!(matches!(signing, Signing::Keeper(_)));
        }
    }

    #[test]
    fn a_testnet_with_neither_the_mnemonic_nor_the_keeper_s_key_only_reads() {
        let signing = Signing::from(NetworkName::Testnet, None, None).unwrap();
        assert!(matches!(signing, Signing::ReadOnly));
        assert!(signing.get(Role::Keeper).is_none());
    }

    #[test]
    fn the_team_s_mnemonic_signs_as_every_role_and_wins_over_the_keeper_s_key() {
        let other = json_of(&keeper_of(DEVELOPMENT_MNEMONIC));
        let signing =
            Signing::from(NetworkName::Testnet, Some(ANOTHER_MNEMONIC), Some(&other)).unwrap();
        assert_eq!(
            signing.get(Role::Keeper).map(Signer::pubkey),
            Some(keeper_of(ANOTHER_MNEMONIC).pubkey())
        );
        assert!(signing.get(Role::Admin).is_some());
    }

    #[test]
    fn the_development_mnemonic_is_refused_on_a_testnet() {
        let refused = Signing::from(
            NetworkName::Testnet,
            Some(&format!(" {DEVELOPMENT_MNEMONIC} ")),
            None,
        );
        assert!(refused.is_err());
        // Locally it is the only phrase, whatever the environment holds.
        let local = Signing::from(NetworkName::Local, Some(ANOTHER_MNEMONIC), None).unwrap();
        assert_eq!(
            local.get(Role::Keeper).map(Signer::pubkey),
            Some(keeper_of(DEVELOPMENT_MNEMONIC).pubkey())
        );
    }

    #[test]
    fn mainnet_only_reads_whatever_the_environment_holds() {
        let key = json_of(&keeper_of(ANOTHER_MNEMONIC));
        let signing =
            Signing::from(NetworkName::Mainnet, Some(ANOTHER_MNEMONIC), Some(&key)).unwrap();
        assert!(matches!(signing, Signing::ReadOnly));
    }

    #[test]
    fn a_keeper_s_own_key_is_refused_unless_it_is_the_deployment_s_keeper() {
        let keeper = keeper_of(ANOTHER_MNEMONIC);
        let signing = Signing::from(NetworkName::Testnet, None, Some(&json_of(&keeper))).unwrap();
        signing.check_keeper(&keeper.pubkey().to_string()).unwrap();
        let other = keeper_of(DEVELOPMENT_MNEMONIC).pubkey().to_string();
        assert!(signing.check_keeper(&other).is_err());
        assert!(signing.check_keeper("").is_err());
        // Signing as every role, or as none, there is no single key to check.
        let roles = Signing::from(NetworkName::Testnet, Some(ANOTHER_MNEMONIC), None).unwrap();
        roles.check_keeper(&other).unwrap();
    }

    #[test]
    fn a_keeper_s_key_that_is_not_a_keypair_is_refused_without_being_quoted() {
        let keeper = keeper_of(ANOTHER_MNEMONIC);
        let mut bytes = keeper.to_bytes();
        // The public half no longer the secret's.
        bytes[40] ^= 1;
        let mismatched = bs58::encode(bytes).into_string();
        let short = bs58::encode(&keeper.to_bytes()[..32]).into_string();
        for text in [mismatched.as_str(), short.as_str(), "[1,2,3]", "zz-junk-zz"] {
            let error = match Signing::from(NetworkName::Testnet, None, Some(text)) {
                Ok(_) => panic!("{text:?} was taken as a key"),
                Err(error) => format!("{error:#}"),
            };
            assert!(!error.contains(text), "the error quotes the key: {error}");
        }
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
