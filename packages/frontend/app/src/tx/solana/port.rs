//! What a Solana flow needs from the outside world, as two small traits: a
//! [`SolReader`] of the cluster (through its public endpoint) and a
//! [`SolSigner`] (the connected wallet). The browser implements them over
//! `fetch` and the wallet; the tests implement them over a simulated
//! cluster, so every flow's sequence is checked natively.

use chain::solana_ix::transaction::Unsigned;
use chain::solana_ix::{Hash, Pubkey};

use super::error::SolError;

/// A transaction's signature: what a Solana transaction is known by.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sig(pub [u8; 64]);

impl Sig {
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = bs58::decode(text).into_vec().ok()?;
        Some(Self(bytes.try_into().ok()?))
    }
}

impl std::fmt::Display for Sig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&bs58::encode(self.0).into_string())
    }
}

impl std::fmt::Debug for Sig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sig({self})")
    }
}

/// An account as the node holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountData {
    pub owner: Pubkey,
    pub lamports: u64,
    pub data: Vec<u8>,
}

/// A recent blockhash, and the last block height a transaction made with it
/// can land at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blockhash {
    pub hash: Hash,
    pub last_valid: u64,
}

/// Why the node refused or failed a transaction, as far as it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// The node's error, as JSON text: `{"InstructionError":[2,{"Custom":6000}]}`.
    pub err: String,
    pub logs: Vec<String>,
}

/// What a simulation found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Simulated {
    Ok { units: u64 },
    Failed(Failure),
}

/// A sent transaction's status, once the node has seen it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub slot: u64,
    /// The node's error, as JSON text, if it failed.
    pub err: Option<String>,
    /// Confirmed (or finalized): a supermajority voted on its block.
    pub confirmed: bool,
}

/// A landed transaction as the node keeps it: what the vault said in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub slot: u64,
    pub err: Option<String>,
    pub logs: Vec<String>,
    /// Every inner call, as (program, data), in order.
    pub calls: Vec<(Pubkey, Vec<u8>)>,
}

/// Reading the cluster.
#[allow(async_fn_in_trait)]
pub trait SolReader {
    /// The cluster's genesis hash, in base58: which cluster the endpoint is.
    async fn genesis(&self) -> Result<String, SolError>;
    async fn account(&self, key: &Pubkey) -> Result<Option<AccountData>, SolError>;
    /// Several accounts, as they stood at one slot.
    async fn accounts(&self, keys: &[Pubkey]) -> Result<Vec<Option<AccountData>>, SolError>;
    /// A program's accounts whose bytes at each offset are those given.
    async fn program_accounts(
        &self,
        program: &Pubkey,
        filters: &[(usize, Vec<u8>)],
    ) -> Result<Vec<(Pubkey, AccountData)>, SolError>;
    async fn blockhash(&self) -> Result<Blockhash, SolError>;
    async fn block_height(&self) -> Result<u64, SolError>;
    /// The cluster's own clock, in seconds (the clock sysvar).
    async fn chain_time(&self) -> Result<u64, SolError>;
    /// Run a transaction (signatures not checked) without sending it.
    async fn simulate(&self, wire: &[u8]) -> Result<Simulated, SolError>;
    /// Send a signed transaction.
    async fn send(&self, wire: &[u8]) -> Result<Sig, SolError>;
    /// A transaction's status; `history` searches past the recent ones.
    async fn status(&self, signature: &Sig, history: bool) -> Result<Option<Status>, SolError>;
    /// Signatures of transactions naming `key`, newest first.
    async fn signatures_for(&self, key: &Pubkey, limit: usize) -> Result<Vec<Sig>, SolError>;
    async fn transaction(&self, signature: &Sig) -> Result<Option<Record>, SolError>;
}

/// What a wallet did with a transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signed {
    /// It sent it itself (`signAndSendTransaction`): its signature.
    Sent(Sig),
    /// It signed it and handed it back for the app to send: the transaction
    /// on the wire, signatures included.
    Wire(Vec<u8>),
}

/// The connected wallet.
#[allow(async_fn_in_trait)]
pub trait SolSigner {
    /// The account it signs for now. A flow made for another account stops.
    fn account(&self) -> Option<Pubkey>;
    /// What the person calls it, for "Approve in Phantom".
    fn name(&self) -> String;
    /// Whether it can act on this cluster (`solana:devnet`).
    fn supports(&self, chain: &str) -> bool;
    /// Sign the transaction for `chain`, and send it or hand it back.
    async fn sign(&self, unsigned: &Unsigned, chain: &str) -> Result<Signed, SolError>;
}
