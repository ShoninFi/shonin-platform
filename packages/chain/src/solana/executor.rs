//! What the executor reads, sends and signs on Solana, beside what
//! [`Platform`] builds for every service.
//!
//! - [`vault_key`]: the key a vault's agent trades with, made where it runs.
//! - [`vaults`]: every vault the vault program holds.
//! - [`Blessed`]: where a key's blessing for a vault stands.
//! - [`venue_withdraw`], [`venue_outcome`]: the stand-in venue's way home,
//!   and what it did with an order.
//! - [`SignedTotal`]: a running usage total as the executor serves it and
//!   the keeper collects it.

use anchor_lang::solana_program::system_instruction;
use anchor_lang::{AccountDeserialize, Discriminator, InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use shonin_registry::read as registry;
use shonin_registry::state::{Blessing, ImageKind, KeyInfo};
use shonin_vault::state::Vault;

use super::client::SolanaClient;
use super::events::TransactionRecord;
use super::platform::{Platform, image_digest};
use super::{AccountMeta, Instruction, Keypair, Pubkey, Signer};

/// The key a vault's agent trades with: an ed25519 key from the executor's
/// image and the vault, so a restart picks the same one back up and the
/// station's approval still stands. Never written down.
pub fn vault_key(vault: &Pubkey) -> Keypair {
    let seed = shonin_common::digest::keccak(&[&image_digest("executor"), vault.as_ref()]);
    Keypair::new_from_array(seed)
}

/// Every vault the vault program holds, with its account.
pub async fn vaults(client: &SolanaClient) -> Result<Vec<(Pubkey, Vault)>> {
    let found = client
        .rpc
        .program_accounts(&shonin_vault::ID, &[(0, Vault::DISCRIMINATOR.to_vec())])
        .await?;
    let mut out = Vec::with_capacity(found.len());
    for (address, account) in found {
        match Vault::try_deserialize(&mut &account.data[..]) {
            Ok(vault) => out.push((address, vault)),
            Err(error) => {
                tracing::debug!(%address, %error, "an account of the vault program that is not a vault")
            }
        }
    }
    Ok(out)
}

/// Every market the stand-in venue has listed.
pub async fn venue_markets(client: &SolanaClient) -> Result<Vec<shonin_local_venue::Market>> {
    let found = client
        .rpc
        .program_accounts(
            &shonin_local_venue::ID,
            &[(0, shonin_local_venue::Market::DISCRIMINATOR.to_vec())],
        )
        .await?;
    let mut markets: Vec<shonin_local_venue::Market> = found
        .into_iter()
        .filter_map(|(_, account)| {
            shonin_local_venue::Market::try_deserialize(&mut &account.data[..]).ok()
        })
        .collect();
    markets.sort_by_key(|market| market.id);
    Ok(markets)
}

/// Where `key`'s blessing as one vault's executor stands on the cluster.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Blessed {
    /// When the blessing for this vault and bundle runs out; zero if there
    /// is none.
    pub until: u64,
    /// Whether the key's record says it is an executor's.
    pub executor: bool,
    /// How long a blessing lasts on this cluster.
    pub ttl: u64,
}

impl Blessed {
    /// Read it.
    pub async fn read(
        client: &SolanaClient,
        platform: &Platform,
        key: &Pubkey,
        vault: &Pubkey,
        bundle: &[u8; 32],
    ) -> Result<Self> {
        let config = platform.registry_config(client).await?;
        let blessing: Option<Blessing> = client
            .load(&registry::blessing_address(
                key,
                &platform.cluster,
                &vault.to_bytes(),
                bundle,
            ))
            .await?;
        let info: Option<KeyInfo> = client.load(&registry::key_address(key)).await?;
        Ok(Self {
            until: blessing.map_or(0, |blessing| blessing.until),
            executor: info.is_some_and(|info| {
                info.kind == ImageKind::Executor
                    && info.vault == vault.to_bytes()
                    && &info.strategy_hash == bundle
            }),
            ttl: config.blessing_ttl,
        })
    }

    /// Whether it holds at `now`.
    pub fn holds(&self, now: u64) -> bool {
        self.executor && now < self.until
    }

    /// Whether it has more than half its life left at `now`: blessing it
    /// again before then would be paying for nothing.
    pub fn for_a_while(&self, now: u64) -> bool {
        self.executor && self.until.saturating_sub(now).saturating_mul(2) > self.ttl
    }
}

/// `from` sends `to` this many lamports.
pub fn ix_fund(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    system_instruction::transfer(from, to, lamports)
}

/// The stand-in venue sends `amount` of `master`'s cash back to `master`'s
/// own dollar account: through a station, the way home. Every market the
/// account holds follows, so what is left is valued.
pub async fn venue_withdraw(
    platform: &Platform,
    client: &SolanaClient,
    master: &Pubkey,
    amount: u64,
) -> Result<Instruction> {
    let mut accounts = shonin_local_venue::accounts::Withdraw {
        master: *master,
        account: shonin_local_venue::account_address(master),
        master_dollars: platform.dollars(master),
        venue: shonin_local_venue::venue_address(),
        vault: shonin_local_venue::vault_address(),
        mint: platform.mint,
        token_program: spl_token::ID,
    }
    .to_account_metas(None);
    accounts.extend(
        platform
            .held_markets(client, master)
            .await?
            .into_iter()
            .map(|market| AccountMeta::new_readonly(market, false)),
    );
    Ok(Instruction {
        program_id: shonin_local_venue::ID,
        accounts,
        data: shonin_local_venue::instruction::Withdraw { amount }.data(),
    })
}

/// What the stand-in venue did with an order for `master`, from the events
/// of the transaction that carried it.
pub enum VenueOutcome {
    Filled(shonin_local_venue::Filled),
    /// Nobody was there at the order's price: the market's answer, not a
    /// refusal.
    Unfilled,
    Rejected(String),
    /// The venue said nothing about it.
    Silent,
}

pub fn venue_outcome(record: &TransactionRecord, master: &Pubkey) -> VenueOutcome {
    if let Some(filled) = record
        .log_events::<shonin_local_venue::Filled>()
        .into_iter()
        .find(|filled| &filled.master == master)
    {
        return VenueOutcome::Filled(filled);
    }
    if record
        .log_events::<shonin_local_venue::Unfilled>()
        .iter()
        .any(|unfilled| &unfilled.master == master)
    {
        return VenueOutcome::Unfilled;
    }
    if let Some(rejected) = record
        .log_events::<shonin_local_venue::Rejected>()
        .into_iter()
        .find(|rejected| &rejected.master == master)
    {
        return VenueOutcome::Rejected(rejected.reason);
    }
    VenueOutcome::Silent
}

// The signed usage total ---------------------------------------------------------------

/// A running usage total the executor signed for one vault, as it serves it
/// on its status port (`GET /usage/{cluster}/{vault}`) and the keeper
/// collects it. Keys and the signature are base58; the total is a decimal
/// string in the dollar's smallest unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTotal {
    pub vault: String,
    pub total: String,
    /// The cluster's clock when it was signed.
    pub as_of: u64,
    pub signer: String,
    pub signature: String,
}

impl SignedTotal {
    /// Sign `total` as of `as_of` for `vault`, whose account names
    /// `vault_cluster`.
    pub fn sign(
        key: &Keypair,
        vault: &Pubkey,
        vault_cluster: &[u8; 32],
        total: u64,
        as_of: u64,
    ) -> Self {
        let digest = shonin_vault::upkeep::usage_digest(vault_cluster, vault, total, as_of);
        Self {
            vault: vault.to_string(),
            total: total.to_string(),
            as_of,
            signer: key.pubkey().to_string(),
            signature: key.sign_message(&digest).to_string(),
        }
    }

    pub fn vault(&self) -> Result<Pubkey> {
        super::rpc::parse_pubkey(&self.vault)
    }

    pub fn signer(&self) -> Result<Pubkey> {
        super::rpc::parse_pubkey(&self.signer)
    }

    pub fn total(&self) -> Result<u64> {
        self.total
            .parse()
            .map_err(|_| anyhow!("the total {:?} is not a whole number", self.total))
    }

    pub fn signature(&self) -> Result<[u8; 64]> {
        signature_from_base58(&self.signature)
    }

    /// What was signed: the vault program's usage digest, which binds the
    /// cluster (as the vault's account names it), the program, the vault,
    /// the total and when.
    pub fn digest(&self, vault_cluster: &[u8; 32]) -> Result<[u8; 32]> {
        Ok(shonin_vault::upkeep::usage_digest(
            vault_cluster,
            &self.vault()?,
            self.total()?,
            self.as_of,
        ))
    }

    /// Whether the signature is the signer's over the digest.
    pub fn verify(&self, vault_cluster: &[u8; 32]) -> bool {
        let (Ok(digest), Ok(signer), Ok(signature)) =
            (self.digest(vault_cluster), self.signer(), self.signature())
        else {
            return false;
        };
        verify_ed25519(&signer, &digest, signature)
    }

    /// The Ed25519 instruction that carries the signature in the keeper's
    /// transaction, ahead of `keep`.
    pub fn ed25519_ix(&self, vault_cluster: &[u8; 32]) -> Result<Instruction> {
        Ok(
            solana_ed25519_program::new_ed25519_instruction_with_signature(
                &self.digest(vault_cluster)?,
                &self.signature()?,
                &self.signer()?.to_bytes(),
            ),
        )
    }

    /// The total as the vault program takes it.
    pub fn usage(&self) -> Result<shonin_vault::upkeep::Usage> {
        Ok(shonin_vault::upkeep::Usage {
            total: self.total()?,
            as_of: self.as_of,
            signer: self.signer()?,
        })
    }
}

/// An ed25519 signature written in base58.
pub fn signature_from_base58(text: &str) -> Result<[u8; 64]> {
    let bytes = bs58::decode(text)
        .into_vec()
        .map_err(|_| anyhow!("the signature is not base58"))?;
    bytes
        .try_into()
        .map_err(|_| anyhow!("an ed25519 signature is 64 bytes"))
}

/// Whether `signature` is `signer`'s ed25519 signature over `message`.
pub fn verify_ed25519(signer: &Pubkey, message: &[u8], signature: [u8; 64]) -> bool {
    // The signature type is the one a keypair signs with; it is reached
    // through a signature of its own rather than named, so this crate needs
    // no dependency for it.
    fn of_the_type_of<T>(_: T, value: T) -> T {
        value
    }
    let probe = Keypair::new_from_array([1; 32]).sign_message(&[]);
    of_the_type_of(probe, signature.into()).verify(signer.as_ref(), message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vault_key_is_the_same_for_the_same_vault_and_differs_between_vaults() {
        let one = Pubkey::new_from_array([1; 32]);
        let two = Pubkey::new_from_array([2; 32]);
        assert_eq!(vault_key(&one).pubkey(), vault_key(&one).pubkey());
        assert_ne!(vault_key(&one).pubkey(), vault_key(&two).pubkey());
    }

    #[test]
    fn a_signed_total_verifies_only_for_what_was_signed() {
        let vault = Pubkey::new_from_array([7; 32]);
        let key = vault_key(&vault);
        let cluster = shonin_common::digest::cluster_value("localnet");
        let signed = SignedTotal::sign(&key, &vault, &cluster, 1_234, 99);
        assert!(signed.verify(&cluster));
        // Read back as the keeper reads it.
        let read: SignedTotal =
            serde_json::from_str(&serde_json::to_string(&signed).unwrap()).unwrap();
        assert!(read.verify(&cluster));
        assert_eq!(read.usage().unwrap().total, 1_234);

        let other_cluster = shonin_common::digest::cluster_value("devnet");
        assert!(!signed.verify(&other_cluster));
        let raised = SignedTotal {
            total: "1235".into(),
            ..signed.clone()
        };
        assert!(!raised.verify(&cluster));
        let later = SignedTotal {
            as_of: 100,
            ..signed.clone()
        };
        assert!(!later.verify(&cluster));
        let elsewhere = SignedTotal {
            vault: Pubkey::new_from_array([8; 32]).to_string(),
            ..signed
        };
        assert!(!elsewhere.verify(&cluster));
    }

    #[test]
    fn a_signed_total_travels_in_an_ed25519_instruction_the_vault_reads() {
        let vault = Pubkey::new_from_array([7; 32]);
        let key = vault_key(&vault);
        let cluster = shonin_common::digest::cluster_value("localnet");
        let signed = SignedTotal::sign(&key, &vault, &cluster, 5, 6);
        let ix = signed.ed25519_ix(&cluster).unwrap();
        let digest = signed.digest(&cluster).unwrap();
        assert!(shonin_common::ed25519::verifies(
            &ix.data,
            key.pubkey().as_ref(),
            &digest,
            None
        ));
    }
}
