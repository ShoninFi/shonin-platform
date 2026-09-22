//! Talking to the chain.
//!
//! Every service reaches the contracts through this one client, so they all
//! read the same addresses out of the same deployment file and encode calls
//! with the same bindings.
//!
//! Nothing here holds a key. The development chain unlocks the accounts it
//! publishes, so a service says which account it is and the node signs for
//! it: transactions through `eth_sendTransaction`, messages through
//! `eth_sign`. That is what makes it a development chain, and it means no key
//! is written down anywhere in this repository or anyone's environment.

use std::time::Duration;

use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, B256, Bytes};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::types::{Filter, Log, TransactionReceipt, TransactionRequest};
use alloy::signers::SignerSync;
use alloy::signers::local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use anyhow::{Context, Result, anyhow, bail};

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

/// A connection to the chain, and the addresses of everything on it.
#[derive(Clone)]
pub struct Client {
    provider: DynProvider,
    pub rpc_url: String,
    pub chain_id: u64,
    pub accounts: Vec<Address>,
}

impl Client {
    /// Connect, waiting for the chain to answer: `task dev` starts everything
    /// at once, so a service usually arrives before the chain does.
    pub async fn connect(rpc_url: &str) -> Result<Self> {
        let url = rpc_url.parse().context("the chain's address")?;
        let provider = ProviderBuilder::new().connect_http(url).erased();

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

        let accounts = provider
            .get_accounts()
            .await
            .context("the chain's accounts")?;
        Ok(Self {
            provider,
            rpc_url: rpc_url.to_string(),
            chain_id,
            accounts,
        })
    }

    /// The account that plays a part on this chain.
    pub fn account(&self, role: Role) -> Result<Address> {
        self.accounts
            .get(role.account_index())
            .copied()
            .ok_or_else(|| {
                anyhow!(
                    "the chain published no account for the {} role",
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
