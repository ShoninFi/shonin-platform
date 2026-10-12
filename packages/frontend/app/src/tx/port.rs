//! What a flow needs from the outside world, as two small traits: a
//! [`Reader`] of one vault chain (through its public endpoint) and a
//! [`Signer`] (the connected wallet). The browser implements them over
//! `fetch` and the wallet's provider; the tests implement them over a
//! simulated chain, so every flow's sequence is checked natively.

use alloy_primitives::{Address, B256, Bytes, U256};

use super::error::TxError;

/// One transaction, as the app asks a wallet to send it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxRequest {
    /// The chain it must land on: checked against the wallet immediately
    /// before it is sent, and signed into it by a local signer.
    pub chain_id: u64,
    pub from: Address,
    pub to: Address,
    pub data: Bytes,
    /// Gas, estimated against the chain beforehand with some headroom.
    pub gas: Option<u64>,
}

/// One log line of a receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Log {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
}

/// A mined transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub hash: B256,
    pub success: bool,
    pub block: u64,
    pub logs: Vec<Log>,
}

/// What the chain knows of a sent transaction that hasn't been mined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    /// The node has it, waiting to be mined, at this nonce.
    Known { nonce: u64 },
    /// The node has never heard of it, or has forgotten it.
    Unknown,
}

/// The statement a depositor signs, as the vault checks it: EIP-712, domain
/// `("Shonin Vault", "1", chainId, vault)`, with the vault's own list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementRequest {
    pub chain_id: u64,
    pub vault: Address,
    pub depositor: Address,
    /// The vault's restricted list as it writes it, read from the vault.
    pub restricted: String,
}

impl StatementRequest {
    /// The digest the vault recovers the signer from.
    pub fn digest(&self) -> B256 {
        chain::statement::digest(self.chain_id, self.vault, self.depositor, &self.restricted)
    }

    /// The typed data a wallet is asked to sign (`eth_signTypedData_v4`).
    pub fn typed_data(&self) -> serde_json::Value {
        chain::statement::typed_data(self.chain_id, self.vault, self.depositor, &self.restricted)
    }
}

/// Reading one vault chain.
#[allow(async_fn_in_trait)]
pub trait Reader {
    /// The chain id the endpoint answers for.
    async fn chain_id(&self) -> Result<u64, TxError>;
    /// `eth_call` at the latest block.
    async fn call(&self, to: Address, data: Bytes) -> Result<Bytes, TxError>;
    /// The chain's own coin, which pays the network fee.
    async fn native_balance(&self, who: Address) -> Result<U256, TxError>;
    /// Gas for a transaction as the chain would run it now; fails with the
    /// revert reason when it would revert.
    async fn estimate_gas(&self, request: &TxRequest) -> Result<u64, TxError>;
    /// What a unit of gas costs at most, about now (the max fee).
    async fn gas_price(&self) -> Result<U256, TxError>;
    async fn receipt(&self, hash: B256) -> Result<Option<Receipt>, TxError>;
    async fn pending(&self, hash: B256) -> Result<Pending, TxError>;
    /// The next nonce of an account, counting only mined transactions.
    async fn mined_nonce(&self, who: Address) -> Result<u64, TxError>;
    /// The latest block's time, in seconds: the chain's own clock.
    async fn chain_time(&self) -> Result<u64, TxError>;
    /// A block's time.
    async fn block_time(&self, block: u64) -> Result<u64, TxError>;
    /// Logs from one contract with this first topic (and optionally a second),
    /// from a block to the latest.
    async fn logs(
        &self,
        address: Address,
        topic0: B256,
        topic1: Option<B256>,
        from_block: u64,
    ) -> Result<Vec<Log>, TxError>;
}

/// The connected wallet.
#[allow(async_fn_in_trait)]
pub trait Signer {
    /// The account it signs for now. A flow made for another account stops.
    fn account(&self) -> Option<Address>;
    /// What the person calls it, for "Approve in MetaMask".
    fn name(&self) -> String;
    /// The chain the wallet is on now, asked of the wallet itself.
    async fn chain_id(&self) -> Result<u64, TxError>;
    /// Sign the deposit statement.
    async fn sign_statement(&self, statement: &StatementRequest) -> Result<Bytes, TxError>;
    /// Send a transaction; the hash once the wallet has handed it on.
    async fn send(&self, request: &TxRequest) -> Result<B256, TxError>;
}

/// Waiting, without a browser in the tests.
#[allow(async_fn_in_trait)]
pub trait Clock {
    async fn sleep(&self, millis: u32);
}
