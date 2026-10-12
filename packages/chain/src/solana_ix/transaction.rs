//! The transaction a wallet signs: a message compiled from instructions,
//! a payer and a recent blockhash, laid out on the wire with a slot for each
//! signature. A browser wallet takes [`Unsigned::wire`] and signs and sends
//! it; a key held in the page signs [`Unsigned::message`] and sends
//! [`Unsigned::signed`].

use std::fmt;

use solana_message::compiled_instruction::CompiledInstruction;
use solana_message::{Message, MessageHeader, v0};

pub use solana_message::AddressLookupTableAccount;

use super::{Hash, Instruction, Pubkey};

/// The most a transaction may weigh on the wire.
pub const PACKET_DATA_SIZE: usize = 1232;

/// Why a transaction can't be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransactionError {
    /// The instructions don't compile into a message with these tables.
    Compile(String),
    /// The transaction is larger than the wire takes: it needs a lookup
    /// table, or fewer instructions.
    TooLarge { size: usize },
    /// Signatures given for a transaction that needs another number.
    Signatures { needed: usize, given: usize },
}

impl fmt::Display for TransactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(why) => write!(f, "the transaction doesn't compile: {why}"),
            Self::TooLarge { size } => write!(
                f,
                "the transaction is {size} bytes, more than the {PACKET_DATA_SIZE} the wire takes"
            ),
            Self::Signatures { needed, given } => {
                write!(f, "the transaction needs {needed} signatures, not {given}")
            }
        }
    }
}

impl std::error::Error for TransactionError {}

/// A compiled transaction waiting for its signatures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsigned {
    message: Vec<u8>,
    signers: Vec<Pubkey>,
}

impl Unsigned {
    /// A legacy transaction: every account named by its address.
    pub fn legacy(
        payer: &Pubkey,
        ixs: &[Instruction],
        blockhash: Hash,
    ) -> Result<Self, TransactionError> {
        let message = Message::new_with_blockhash(ixs, Some(payer), &blockhash);
        let signers =
            message.account_keys[..message.header.num_required_signatures as usize].to_vec();
        Self::checked(legacy_bytes(&message), signers)
    }

    /// A version 0 transaction: the accounts `tables` hold named through
    /// them, except signers and the programs called, which the message
    /// always names by address.
    pub fn v0(
        payer: &Pubkey,
        ixs: &[Instruction],
        tables: &[AddressLookupTableAccount],
        blockhash: Hash,
    ) -> Result<Self, TransactionError> {
        let message = v0::Message::try_compile(payer, ixs, tables, blockhash)
            .map_err(|error| TransactionError::Compile(error.to_string()))?;
        let signers =
            message.account_keys[..message.header.num_required_signatures as usize].to_vec();
        Self::checked(v0_bytes(&message), signers)
    }

    fn checked(message: Vec<u8>, signers: Vec<Pubkey>) -> Result<Self, TransactionError> {
        let unsigned = Self { message, signers };
        let size = unsigned.wire().len();
        if size > PACKET_DATA_SIZE {
            return Err(TransactionError::TooLarge { size });
        }
        Ok(unsigned)
    }

    /// The message's bytes: what every signer signs.
    pub fn message(&self) -> &[u8] {
        &self.message
    }

    /// Who must sign, in the order their signatures go: the payer first.
    pub fn signers(&self) -> &[Pubkey] {
        &self.signers
    }

    /// The transaction on the wire with every signature zero, for a wallet
    /// to sign (`signTransaction`, `signAndSendTransaction`).
    pub fn wire(&self) -> Vec<u8> {
        self.with(&vec![[0u8; 64]; self.signers.len()])
    }

    /// The transaction on the wire with `signatures`, one per signer in
    /// [`Unsigned::signers`]'s order.
    pub fn signed(&self, signatures: &[[u8; 64]]) -> Result<Vec<u8>, TransactionError> {
        if signatures.len() != self.signers.len() {
            return Err(TransactionError::Signatures {
                needed: self.signers.len(),
                given: signatures.len(),
            });
        }
        Ok(self.with(signatures))
    }

    fn with(&self, signatures: &[[u8; 64]]) -> Vec<u8> {
        let mut out = Vec::with_capacity(3 + 64 * signatures.len() + self.message.len());
        short_vec(signatures.len(), &mut out);
        for signature in signatures {
            out.extend_from_slice(signature);
        }
        out.extend_from_slice(&self.message);
        out
    }
}

// The message on the wire --------------------------------------------------------------
//
// Written here rather than through the SDK's bincode serialization, whose
// dependencies carry JavaScript bindings the browser build can't link. The
// tests check both against the SDK's own.

fn header(header: &MessageHeader, out: &mut Vec<u8>) {
    out.extend_from_slice(&[
        header.num_required_signatures,
        header.num_readonly_signed_accounts,
        header.num_readonly_unsigned_accounts,
    ]);
}

fn keys(keys: &[Pubkey], out: &mut Vec<u8>) {
    short_vec(keys.len(), out);
    for key in keys {
        out.extend_from_slice(key.as_ref());
    }
}

fn instructions(instructions: &[CompiledInstruction], out: &mut Vec<u8>) {
    short_vec(instructions.len(), out);
    for ix in instructions {
        out.push(ix.program_id_index);
        short_vec(ix.accounts.len(), out);
        out.extend_from_slice(&ix.accounts);
        short_vec(ix.data.len(), out);
        out.extend_from_slice(&ix.data);
    }
}

/// A legacy message's bytes, as the SDK's `Message::serialize` writes them.
fn legacy_bytes(message: &Message) -> Vec<u8> {
    let mut out = Vec::new();
    header(&message.header, &mut out);
    keys(&message.account_keys, &mut out);
    out.extend_from_slice(message.recent_blockhash.as_ref());
    instructions(&message.instructions, &mut out);
    out
}

/// A version 0 message's bytes, as the SDK's `VersionedMessage::serialize`
/// writes them: the version prefix, then the legacy fields, then the tables.
fn v0_bytes(message: &v0::Message) -> Vec<u8> {
    let mut out = vec![0x80];
    header(&message.header, &mut out);
    keys(&message.account_keys, &mut out);
    out.extend_from_slice(message.recent_blockhash.as_ref());
    instructions(&message.instructions, &mut out);
    short_vec(message.address_table_lookups.len(), &mut out);
    for lookup in &message.address_table_lookups {
        out.extend_from_slice(lookup.account_key.as_ref());
        short_vec(lookup.writable_indexes.len(), &mut out);
        out.extend_from_slice(&lookup.writable_indexes);
        short_vec(lookup.readonly_indexes.len(), &mut out);
        out.extend_from_slice(&lookup.readonly_indexes);
    }
    out
}

/// A length as the wire writes it: seven bits a byte, low first, the top bit
/// set on every byte but the last.
pub(super) fn short_vec(mut len: usize, out: &mut Vec<u8>) {
    loop {
        let byte = (len & 0x7f) as u8;
        len >>= 7;
        if len == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}
