//! The recovery-phrase wallet: a key derived from a BIP-39 phrase
//! (`m/44'/60'/0'/0/0`, as every EVM wallet derives its first account),
//! held in this page's memory only and signing here. Nothing of it is ever
//! stored, logged or sent: a reload forgets it, and only what it signs
//! leaves the page.
//!
//! What is wiped: the key's bytes when it is dropped, and the app's own
//! copies of the phrase (as typed, and as it is read) as soon as they are
//! used, the phrase a refused try reports included. What is not: the
//! BIP-39 library the key is derived with keeps short-lived copies of its
//! own (the phrase it rebuilds to check the checksum and to derive the
//! seed, the entropy, and the builder's copy of the phrase) and frees them
//! without wiping, so they may stay in the page's memory until it reuses
//! that memory or the page is closed.

use alloy_consensus::{SignableTransaction, TxEip1559, TxLegacy};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::coins_bip39::English;
use alloy_signer_local::{MnemonicBuilder, PrivateKeySigner};
use zeroize::Zeroizing;

/// The public label the probe signer's secret is derived from.
const PROBE_SIGNER: &[u8] = b"shonin: l1 data fee probe signer, holds nothing";

/// A key held in memory.
#[derive(Clone)]
pub struct LocalKey {
    signer: PrivateKeySigner,
}

impl std::fmt::Debug for LocalKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key: only whose it is.
        write!(f, "LocalKey({:?})", self.signer.address())
    }
}

/// Why a phrase didn't give a key. Never carries the phrase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadPhrase;

impl BadPhrase {
    pub fn words(&self) -> &'static str {
        "That isn't a valid recovery phrase. Check the words and their order."
    }
}

/// A phrase's words in lower case, one space apart, if there are as many
/// as a phrase has. Every copy made on the way is wiped when it is dropped,
/// and so is what this returns.
pub(crate) fn normalized(phrase: &str) -> Result<Zeroizing<String>, BadPhrase> {
    let words: Zeroizing<Vec<String>> = Zeroizing::new(
        phrase
            .split_whitespace()
            .map(|word| word.to_lowercase())
            .collect(),
    );
    if !matches!(words.len(), 12 | 15 | 18 | 21 | 24) {
        return Err(BadPhrase);
    }
    let mut out = Zeroizing::new(String::with_capacity(
        words.iter().map(|word| word.len() + 1).sum(),
    ));
    for (index, word) in words.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(word);
    }
    Ok(out)
}

/// A refusal from the BIP-39 library, dropped as [`BadPhrase`]: the copy
/// of the typed words a refusal carries (the phrase, refused on its
/// checksum, or a word not in the list) is wiped.
pub(crate) fn wipe_refusal(error: alloy_signer_local::coins_bip39::MnemonicError) -> BadPhrase {
    use alloy_signer_local::coins_bip39::{MnemonicError, WordlistError};
    use zeroize::Zeroize;
    match error {
        MnemonicError::InvalidPhrase(mut words)
        | MnemonicError::WordlistError(WordlistError::InvalidWord(mut words)) => words.zeroize(),
        _ => {}
    }
    BadPhrase
}

/// The phrase as it is typed, before it is tried: wiped whenever it is
/// replaced, taken or dropped.
#[derive(Default)]
pub struct HeldPhrase(std::cell::RefCell<Zeroizing<String>>);

impl HeldPhrase {
    /// Hold what the field says now, wiping what was held before.
    pub fn set(&self, typed: String) {
        *self.0.borrow_mut() = Zeroizing::new(typed);
    }

    /// Try the held phrase with `connect`, leaving nothing held whatever
    /// the answer: the words are wiped as soon as `connect` returns.
    pub fn submit(
        &self,
        connect: impl FnOnce(&str) -> Result<(), BadPhrase>,
    ) -> Result<(), BadPhrase> {
        let words = std::mem::take(&mut *self.0.borrow_mut());
        connect(&words)
    }

    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }
}

impl LocalKey {
    /// The first account of a phrase. Spaces and case don't matter; the
    /// words and their checksum do.
    pub fn from_phrase(phrase: &str) -> Result<Self, BadPhrase> {
        let words = normalized(phrase)?;
        let refused = |error: alloy_signer_local::LocalSignerError| {
            if let alloy_signer_local::LocalSignerError::Bip39Error(error) = error {
                wipe_refusal(error);
            }
            BadPhrase
        };
        let signer = MnemonicBuilder::<English>::default()
            .phrase(words.as_str())
            .index(0u32)
            .map_err(refused)?
            .build()
            .map_err(refused)?;
        Ok(Self { signer })
    }

    /// The key that signs the L1 data fee probe: never the person's, so
    /// the bytes handed to the endpoint are never a send the person signed.
    /// Its secret is the keccak of a public label, so anyone can derive it;
    /// it holds nothing, so whatever it signs is sent, if anyone ever sends
    /// it, from its own address and never spends the person's funds.
    pub(crate) fn probe_signer() -> Self {
        let secret = alloy_primitives::keccak256(PROBE_SIGNER);
        let signer = PrivateKeySigner::from_bytes(&secret)
            .expect("the probe label's keccak is a valid secp256k1 secret");
        Self { signer }
    }

    pub fn address(&self) -> Address {
        self.signer.address()
    }

    /// Sign a 32-byte digest (the statement's EIP-712 digest): 65 bytes,
    /// `r ‖ s ‖ v` with `v` 27 or 28, as the vault's signature check takes it.
    pub fn sign_digest(&self, digest: B256) -> Bytes {
        let signature = self
            .signer
            .sign_hash_sync(&digest)
            .expect("a local key signs any digest");
        Bytes::from(signature.as_bytes().to_vec())
    }

    /// Sign a transaction for `chain_id` and encode it for
    /// `eth_sendRawTransaction`.
    pub fn sign_transaction(&self, tx: &Unsigned) -> Bytes {
        let mut out = Vec::new();
        match tx.fees {
            Fees::Eip1559 {
                max_fee,
                max_priority_fee,
            } => {
                let tx = TxEip1559 {
                    chain_id: tx.chain_id,
                    nonce: tx.nonce,
                    gas_limit: tx.gas,
                    max_fee_per_gas: max_fee,
                    max_priority_fee_per_gas: max_priority_fee,
                    to: TxKind::Call(tx.to),
                    value: U256::ZERO,
                    access_list: Default::default(),
                    input: tx.data.clone(),
                };
                let signature = self
                    .signer
                    .sign_hash_sync(&tx.signature_hash())
                    .expect("a local key signs any digest");
                tx.into_signed(signature).encode_2718(&mut out);
            }
            Fees::Legacy { gas_price } => {
                let tx = TxLegacy {
                    chain_id: Some(tx.chain_id),
                    nonce: tx.nonce,
                    gas_price,
                    gas_limit: tx.gas,
                    to: TxKind::Call(tx.to),
                    value: U256::ZERO,
                    input: tx.data.clone(),
                };
                let signature = self
                    .signer
                    .sign_hash_sync(&tx.signature_hash())
                    .expect("a local key signs any digest");
                tx.into_signed(signature).encode_2718(&mut out);
            }
        }
        Bytes::from(out)
    }
}

/// How a transaction pays for gas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fees {
    Eip1559 {
        max_fee: u128,
        max_priority_fee: u128,
    },
    Legacy {
        gas_price: u128,
    },
}

/// A transaction ready to sign: the chain id is signed into it (EIP-155), so
/// it can only ever land on that chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsigned {
    pub chain_id: u64,
    pub nonce: u64,
    pub gas: u64,
    pub fees: Fees,
    pub to: Address,
    pub data: Bytes,
}

#[cfg(test)]
mod tests {
    use alloy_consensus::TxEnvelope;
    use alloy_consensus::transaction::SignerRecoverable;
    use alloy_eips::eip2718::Decodable2718;

    use super::*;

    /// The phrase every development chain publishes for its test accounts.
    const PUBLIC_TEST_PHRASE: &str = "test test test test test test test test test test test junk";

    #[test]
    fn a_phrase_tried_is_never_held_after_whether_it_is_taken_or_refused() {
        let held = HeldPhrase::default();
        held.set("not a phrase at all".to_string());
        assert!(!held.is_empty());
        let mut seen = String::new();
        let refused = held.submit(|words| {
            seen = words.to_string();
            LocalKey::from_phrase(words).map(|_| ())
        });
        assert_eq!(refused, Err(BadPhrase));
        assert_eq!(seen, "not a phrase at all");
        assert!(held.is_empty());

        held.set(PUBLIC_TEST_PHRASE.to_string());
        let try_it = |words: &str| LocalKey::from_phrase(words).map(|_| ());
        assert_eq!(held.submit(try_it), Ok(()));
        assert!(held.is_empty());
        // Nothing is left to try: an empty phrase is refused, not the old one.
        assert_eq!(held.submit(try_it), Err(BadPhrase));
    }

    #[test]
    fn a_phrase_is_read_in_lower_case_one_space_apart() {
        let typed = format!(
            "  {}  ",
            PUBLIC_TEST_PHRASE.to_uppercase().replace(' ', "   ")
        );
        assert_eq!(normalized(&typed).unwrap().as_str(), PUBLIC_TEST_PHRASE);
        assert_eq!(normalized("one two three").unwrap_err(), BadPhrase);
    }

    #[test]
    fn a_phrase_gives_its_first_account_as_every_wallet_derives_it() {
        let key = LocalKey::from_phrase(PUBLIC_TEST_PHRASE).unwrap();
        assert_eq!(
            key.address(),
            "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
                .parse::<Address>()
                .unwrap()
        );
        // Spaces and case don't matter.
        let messy = format!(
            "  {}  ",
            PUBLIC_TEST_PHRASE.to_uppercase().replace(' ', "   ")
        );
        assert_eq!(
            LocalKey::from_phrase(&messy).unwrap().address(),
            key.address()
        );
    }

    #[test]
    fn a_wrong_phrase_is_refused_without_repeating_it() {
        assert_eq!(
            LocalKey::from_phrase("test test test test test test test test test test test test")
                .unwrap_err(),
            BadPhrase
        );
        assert_eq!(LocalKey::from_phrase("").unwrap_err(), BadPhrase);
        assert_eq!(
            LocalKey::from_phrase("one two three").unwrap_err(),
            BadPhrase
        );
        assert!(!BadPhrase.words().contains("test"));
    }

    #[test]
    fn the_key_never_shows_in_debug_output() {
        let key = LocalKey::from_phrase(PUBLIC_TEST_PHRASE).unwrap();
        let shown = format!("{key:?}");
        assert!(shown.starts_with("LocalKey(0x"));
        // An address is 40 hex digits; a key would be 64.
        let longest_hex = shown
            .split(|c: char| !c.is_ascii_hexdigit())
            .map(str::len)
            .max()
            .unwrap_or_default();
        assert!(longest_hex <= 40, "{shown}");
        assert!(!shown.contains("test"));
    }

    #[test]
    fn a_signed_digest_recovers_to_the_account() {
        let key = LocalKey::from_phrase(PUBLIC_TEST_PHRASE).unwrap();
        let digest = B256::repeat_byte(5);
        let signature = key.sign_digest(digest);
        assert_eq!(signature.len(), 65);
        assert!(signature[64] == 27 || signature[64] == 28);
        assert!(crate::tx::flow::recovers_to(
            &signature,
            digest,
            key.address()
        ));
    }

    #[test]
    fn a_signed_transaction_names_its_chain_and_recovers_to_the_account() {
        let key = LocalKey::from_phrase(PUBLIC_TEST_PHRASE).unwrap();
        for fees in [
            Fees::Eip1559 {
                max_fee: 2_000_000_000,
                max_priority_fee: 1_000_000,
            },
            Fees::Legacy {
                gas_price: 1_000_000_000,
            },
        ] {
            let unsigned = Unsigned {
                chain_id: 84532,
                nonce: 7,
                gas: 100_000,
                fees,
                to: Address::repeat_byte(0xaa),
                data: Bytes::from(vec![1, 2, 3]),
            };
            let raw = key.sign_transaction(&unsigned);
            let envelope = TxEnvelope::decode_2718(&mut raw.as_ref()).unwrap();
            assert_eq!(envelope.recover_signer().unwrap(), key.address());
            assert_eq!(
                alloy_consensus::Transaction::chain_id(&envelope),
                Some(84532)
            );
            assert_eq!(alloy_consensus::Transaction::nonce(&envelope), 7);
            assert_eq!(
                alloy_consensus::Transaction::to(&envelope),
                Some(Address::repeat_byte(0xaa))
            );
            assert_eq!(alloy_consensus::Transaction::value(&envelope), U256::ZERO);
            assert_eq!(
                alloy_consensus::Transaction::input(&envelope).as_ref(),
                &[1, 2, 3]
            );
        }
    }
}
