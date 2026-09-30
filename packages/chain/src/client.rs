//! Talking to the chain.
//!
//! Every service reaches the contracts through this one client, so they all
//! read the same addresses out of the same deployment file and encode calls
//! with the same bindings.
//!
//! How a client signs depends on the chain ([`Signing`]). A local chain
//! unlocks the accounts it publishes, so a service says which account it is
//! and the node signs for it: transactions through `eth_sendTransaction`,
//! messages through `eth_sign`. That is what makes it a development chain,
//! and it means no key is written down anywhere. On a testnet the same roles
//! are accounts derived from a mnemonic the developer holds in `.env`, signed
//! for here. Mainnet has no role keys at all: a client there only reads.

use std::time::Duration;

use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, B256, Bytes};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::types::{Filter, Log, TransactionReceipt, TransactionRequest};
use alloy::signers::SignerSync;
use alloy::signers::local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use anyhow::{Context, Result, anyhow, bail};

use std::collections::HashMap;
use std::sync::Arc;

use crate::config::ChainConfig;
use crate::deployments::Deployments;

/// One of the platform's roles, as an account on the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Admin,
    Platform,
    Attestor,
    Builder,
    Reviewer,
    Executor,
    Keeper,
    Operator,
    Author,
    Viewer,
}

impl Role {
    /// Which of the accounts the chain publishes plays this part.
    pub fn account_index(self) -> usize {
        match self {
            Self::Admin => 0,
            Self::Builder => 1,
            Self::Reviewer => 2,
            Self::Executor => 3,
            Self::Keeper => 4,
            Self::Author => 5,
            Self::Viewer => 6,
            Self::Operator => 7,
            Self::Attestor => 8,
            Self::Platform => 9,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Platform => "platform",
            Self::Attestor => "attestor",
            Self::Builder => "builder",
            Self::Reviewer => "reviewer",
            Self::Executor => "executor",
            Self::Keeper => "keeper",
            Self::Operator => "operator",
            Self::Author => "author",
            Self::Viewer => "viewer",
        }
    }
}

/// Who signs a client's transactions and messages.
pub enum Signing {
    /// The node, for the accounts it unlocked: a development chain.
    Unlocked,
    /// These keys, in role order, signed for here.
    Keys(Vec<PrivateKeySigner>),
    /// Nobody: the client only reads.
    ReadOnly,
}

/// The first accounts a mnemonic derives (`m/44'/60'/0'/0/i`), one per role
/// index, the same way a development chain derives its own.
pub fn keys_from_mnemonic(phrase: &str, count: usize) -> Result<Vec<PrivateKeySigner>> {
    use alloy::signers::local::MnemonicBuilder;
    use alloy::signers::local::coins_bip39::English;
    (0..count as u32)
        .map(|index| {
            MnemonicBuilder::<English>::default()
                .phrase(phrase.trim())
                .index(index)?
                .build()
                .context("deriving an account from the mnemonic")
        })
        .collect()
}

/// How many role accounts there are ([`Role::account_index`]).
pub const ROLE_COUNT: usize = 10;

/// A connection to the chain, and the addresses of everything on it.
#[derive(Clone)]
pub struct Client {
    provider: DynProvider,
    pub rpc_url: String,
    pub chain_id: u64,
    pub accounts: Vec<Address>,
    /// The keys this client signs with itself, when the node doesn't.
    keys: Option<Arc<HashMap<Address, PrivateKeySigner>>>,
}

impl Client {
    /// Connect, waiting for the chain to answer: `task dev` starts everything
    /// at once, so a service usually arrives before the chain does.
    pub async fn connect(config: &ChainConfig, signing: Signing) -> Result<Self> {
        let rpc_url = config.rpc.as_str();
        let url = rpc_url.parse().context("the chain's address")?;
        let provider = match &signing {
            Signing::Keys(keys) if !keys.is_empty() => {
                let mut wallet = EthereumWallet::from(keys[0].clone());
                for key in &keys[1..] {
                    wallet.register_signer(key.clone());
                }
                ProviderBuilder::new()
                    .wallet(wallet)
                    .connect_http(url)
                    .erased()
            }
            _ => ProviderBuilder::new().connect_http(url).erased(),
        };

        let mut waited = Duration::ZERO;
        let chain_id = loop {
            match provider.get_chain_id().await {
                Ok(id) => break id,
                Err(error) => {
                    if waited > Duration::from_secs(120) {
                        return Err(anyhow!("the chain never answered at {rpc_url}: {error}"));
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    waited += Duration::from_millis(500);
                }
            }
        };

        let (accounts, keys) = match signing {
            Signing::Unlocked => (
                provider
                    .get_accounts()
                    .await
                    .context("the chain's accounts")?,
                None,
            ),
            Signing::Keys(keys) => (
                keys.iter().map(|key| key.address()).collect(),
                Some(Arc::new(
                    keys.into_iter().map(|key| (key.address(), key)).collect(),
                )),
            ),
            Signing::ReadOnly => (Vec::new(), None),
        };
        Ok(Self {
            provider,
            rpc_url: rpc_url.to_string(),
            chain_id,
            accounts,
            keys,
        })
    }

    /// Whether the node itself holds the accounts' keys, which is what makes
    /// it a development chain.
    pub fn is_development(&self) -> bool {
        self.keys.is_none() && !self.accounts.is_empty()
    }

    /// Send the chain's own coin, for gas.
    pub async fn transfer(
        &self,
        from: Address,
        to: Address,
        value: alloy::primitives::U256,
    ) -> Result<TransactionReceipt> {
        let request = TransactionRequest::default()
            .with_from(from)
            .with_to(to)
            .with_value(value);
        let receipt = self
            .provider
            .send_transaction(request)
            .await
            .context("sending gas money")?
            .get_receipt()
            .await
            .context("waiting for a transfer")?;
        if !receipt.status() {
            bail!("the transfer to {to} was reverted by the chain");
        }
        Ok(receipt)
    }

    /// The chain's own coin an address holds.
    pub async fn balance(&self, of: Address) -> Result<alloy::primitives::U256> {
        self.provider
            .get_balance(of)
            .await
            .context("reading a balance")
    }

    /// The account that plays a part on this chain.
    pub fn account(&self, role: Role) -> Result<Address> {
        self.accounts
            .get(role.account_index())
            .copied()
            .ok_or_else(|| {
                anyhow!(
                    "no account plays the {} role here: on a testnet, set SHONIN_TESTNET_EVM_MNEMONIC; on mainnet, clients only read",
                    role.label()
                )
            })
    }

    pub fn provider(&self) -> &DynProvider {
        &self.provider
    }

    /// Read a contract, without sending anything.
    pub async fn read<C: SolCall>(&self, to: Address, call: C) -> Result<C::Return> {
        let request = TransactionRequest::default()
            .with_to(to)
            .with_input(call.abi_encode());
        let returned = self
            .provider
            .call(request)
            .await
            .with_context(|| format!("reading {} at {to}", C::SIGNATURE))?;
        C::abi_decode_returns(&returned)
            .with_context(|| format!("decoding what {} returned", C::SIGNATURE))
    }

    /// Read a contract as it was at a past block. A chain that keeps its
    /// history is a record nobody has to be trusted to have kept.
    pub async fn read_at<C: SolCall>(
        &self,
        to: Address,
        call: C,
        block: alloy::eips::BlockId,
    ) -> Result<C::Return> {
        let request = TransactionRequest::default()
            .with_to(to)
            .with_input(call.abi_encode());
        let returned = self
            .provider
            .call(request)
            .block(block)
            .await
            .with_context(|| format!("reading {} at block {block:?}", C::SIGNATURE))?;
        C::abi_decode_returns(&returned)
            .with_context(|| format!("decoding what {} returned", C::SIGNATURE))
    }

    /// When a past block was mined, in the chain's own time.
    pub async fn block_timestamp_at(&self, block: u64) -> Result<u64> {
        let block = self
            .provider
            .get_block(alloy::eips::BlockId::number(block))
            .await
            .context("a past block")?
            .ok_or_else(|| anyhow!("no such block"))?;
        Ok(block.header.timestamp)
    }

    /// Send a call as `from`, and wait for it to land.
    pub async fn send<C: SolCall>(
        &self,
        from: Address,
        to: Address,
        call: C,
    ) -> Result<TransactionReceipt> {
        let request = TransactionRequest::default()
            .with_from(from)
            .with_to(to)
            .with_input(call.abi_encode());
        let pending = self
            .provider
            .send_transaction(request)
            .await
            .with_context(|| format!("sending {}", C::SIGNATURE))?;
        let receipt = pending
            .get_receipt()
            .await
            .with_context(|| format!("waiting for {}", C::SIGNATURE))?;
        if !receipt.status() {
            bail!("{} was reverted by the chain", C::SIGNATURE);
        }
        Ok(receipt)
    }

    /// Deploy a contract from its bytecode and constructor arguments.
    pub async fn deploy(&self, from: Address, bytecode: Vec<u8>) -> Result<Address> {
        let request = TransactionRequest::default()
            .with_from(from)
            .with_deploy_code(bytecode);
        let receipt = self
            .provider
            .send_transaction(request)
            .await
            .context("deploying")?
            .get_receipt()
            .await
            .context("waiting for a deployment")?;
        receipt
            .contract_address
            .ok_or_else(|| anyhow!("the chain accepted a deployment without giving it an address"))
    }

    /// Have the node sign a digest as `who`, the way a wallet would: the
    /// signature a contract recovers with `toEthSignedMessageHash`.
    pub async fn sign(&self, who: Address, digest: B256) -> Result<Bytes> {
        if let Some(key) = self.keys.as_ref().and_then(|keys| keys.get(&who)) {
            let signature = key
                .sign_message_sync(digest.as_slice())
                .context("signing")?;
            return Ok(Bytes::from(signature.as_bytes().to_vec()));
        }
        let signature: Bytes = self
            .provider
            .raw_request("eth_sign".into(), (who, Bytes::from(digest.to_vec())))
            .await
            .with_context(|| format!("asking the chain to sign as {who}"))?;
        Ok(signature)
    }

    /// Ask the node something the contracts do not cover, such as how fast a
    /// development chain should run its clock.
    pub async fn raw_request(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.provider
            .raw_request(method.into(), params)
            .await
            .with_context(|| format!("asking the chain for {method}"))
    }

    pub async fn logs(&self, filter: &Filter) -> Result<Vec<Log>> {
        self.provider.get_logs(filter).await.context("reading logs")
    }

    pub async fn block_number(&self) -> Result<u64> {
        self.provider.get_block_number().await.context("the head")
    }

    /// The chain's own clock. Everything the platform dates is relative to
    /// this, never to the reader's watch.
    pub async fn block_timestamp(&self) -> Result<u64> {
        let block = self
            .provider
            .get_block(alloy::eips::BlockId::latest())
            .await
            .context("the head block")?
            .ok_or_else(|| anyhow!("the chain has no blocks yet"))?;
        Ok(block.header.timestamp)
    }

    /// Whether anything is deployed at an address: how a deploy knows the
    /// file it found is describing a chain that still exists.
    pub async fn has_code(&self, at: Address) -> Result<bool> {
        let code = self
            .provider
            .get_code_at(at)
            .await
            .context("reading a contract's code")?;
        Ok(!code.is_empty())
    }

    /// A client that acts as a key the service generated itself, rather than
    /// as one of the accounts the chain published.
    ///
    /// An executor's trading keys are like this: they are made where the
    /// strategy runs, they are blessed for one vault, and nothing outside that
    /// enclave ever holds them.
    pub fn with_key(&self, signer: PrivateKeySigner) -> Result<KeyClient> {
        let address = signer.address();
        let url = self.rpc_url.parse().context("the chain's address")?;
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(signer.clone()))
            .connect_http(url)
            .erased();
        Ok(KeyClient {
            provider,
            signer,
            address,
        })
    }

    /// Read the deployment file, and check it describes this chain.
    pub async fn deployments(&self, path: &std::path::Path) -> Result<Deployments> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let deployments = Deployments::from_json(&text).context("the deployment file")?;
        if deployments.chain_id != self.chain_id {
            bail!(
                "the deployment file is for chain {} and this chain is {}",
                deployments.chain_id,
                self.chain_id
            );
        }
        Ok(deployments)
    }

    /// Wait until a deploy has written the file and its contracts are there.
    pub async fn wait_for_deployments(&self, path: &std::path::Path) -> Result<Deployments> {
        let mut waited = Duration::ZERO;
        loop {
            if path.exists()
                && let Ok(deployments) = self.deployments(path).await
                && self.has_code(deployments.factory).await.unwrap_or(false)
            {
                return Ok(deployments);
            }
            if waited > Duration::from_secs(180) {
                bail!("no deployment appeared at {}", path.display());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            waited += Duration::from_millis(500);
        }
    }
}

/// The chain, as seen by a key that signs for itself.
#[derive(Clone)]
pub struct KeyClient {
    provider: DynProvider,
    signer: PrivateKeySigner,
    pub address: Address,
}

impl KeyClient {
    pub async fn send<C: SolCall>(&self, to: Address, call: C) -> Result<TransactionReceipt> {
        let request = TransactionRequest::default()
            .with_to(to)
            .with_input(call.abi_encode());
        let receipt = self
            .provider
            .send_transaction(request)
            .await
            .with_context(|| format!("sending {}", C::SIGNATURE))?
            .get_receipt()
            .await
            .with_context(|| format!("waiting for {}", C::SIGNATURE))?;
        if !receipt.status() {
            bail!("{} was reverted by the chain", C::SIGNATURE);
        }
        Ok(receipt)
    }

    /// Sign a digest the way a contract expects to recover it.
    pub fn sign(&self, digest: B256) -> Result<Bytes> {
        let signature = self
            .signer
            .sign_message_sync(digest.as_slice())
            .context("signing")?;
        Ok(Bytes::from(signature.as_bytes().to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mnemonic_derives_the_accounts_a_development_chain_would() {
        // Anvil's published mnemonic, and the first two accounts it hands out.
        let keys = keys_from_mnemonic(
            "test test test test test test test test test test test junk",
            2,
        )
        .unwrap();
        assert_eq!(
            format!("{:?}", keys[0].address()),
            "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
        );
        assert_eq!(
            format!("{:?}", keys[1].address()),
            "0x70997970c51812dc3a010c7d01b50e0d17dc79c8"
        );
    }
}
