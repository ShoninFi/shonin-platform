//! Funding a person's own wallet on the testnets, so the web app's money
//! flows can be tried with it: the stand-in dollar, and with `--gas` the
//! chain's coin.
//!
//! The dollar is minted the way the seed mints it for its own accounts. On an
//! EVM chain anyone may mint the stand-in dollar, and the funder (which holds
//! the gas) sends the mint. On a Solana cluster the dollar's mint authority is
//! the admin, which opens the wallet's dollar account if it isn't there and
//! mints into it. The coin comes from each chain's funder (the EVM funder, the
//! Solana deployer), never past the reserve `config/funding.toml` keeps for
//! it. Both are top-ups: a wallet already holding its amount is left alone,
//! so running it again sends nothing new. Testnet only.

use std::path::Path;
use std::time::Duration;

use alloy::network::TransactionBuilder;
use alloy::primitives::utils::format_ether;
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol_types::SolCall;
use anyhow::{Context, Result, anyhow, bail};
use chain::bindings::usd::ILocalUsd;
use chain::solana::{Pubkey, Signer};
use chain::{ChainConfig, ChainKey, Deployments, NetworkName, Role};

use crate::fund::{self, Funding};

/// Who is funded: an EVM address on the EVM chains, a Solana one on the
/// Solana clusters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wallet {
    Evm(Address),
    Solana(Pubkey),
}

impl Wallet {
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.starts_with("0x") {
            return text
                .parse()
                .map(Self::Evm)
                .map_err(|_| anyhow!("{text} is not an EVM address"));
        }
        text.parse()
            .map(Self::Solana)
            .map_err(|_| anyhow!("{text} is neither an EVM address (0x...) nor a Solana one"))
    }
}

/// What the wallet is topped up to on each chain.
pub struct Amounts {
    /// Whole dollars of the stand-in dollar.
    pub dollars: u64,
    /// Whether to top its coin up to the chain's `default` in the funding
    /// file, from the chain's funder.
    pub gas: bool,
}

/// Fund `wallet` on every chain of its kind in `chains`.
pub async fn run(
    network: NetworkName,
    deployments_dir: &Path,
    chains: Vec<(ChainKey, ChainConfig)>,
    funding_file: &Path,
    wallet: Wallet,
    amounts: &Amounts,
) -> Result<()> {
    if network != NetworkName::Testnet {
        bail!(
            "fund-wallet funds a wallet on the testnets, and runs only with NETWORK=testnet (the local chains' development wallet holds its own)"
        );
    }
    let funding = Funding::read(funding_file)?;
    if chains.is_empty() {
        bail!("the {network} network has no chain of this address's kind");
    }
    let mut failed = Vec::new();
    for (key, config) in chains {
        let file = Deployments::file_in(deployments_dir, &key);
        let outcome = match wallet {
            Wallet::Evm(address) => {
                evm(network, &key, &config, &file, &funding, address, amounts).await
            }
            Wallet::Solana(address) => {
                solana(network, &key, &config, &file, &funding, address, amounts).await
            }
        };
        if let Err(error) = outcome {
            println!("  failed: {error:#}");
            failed.push(key.to_string());
        }
    }
    if !failed.is_empty() {
        bail!("funding the wallet failed on {}", failed.join(", "));
    }
    Ok(())
}

async fn evm(
    network: NetworkName,
    key: &ChainKey,
    config: &ChainConfig,
    file: &Path,
    funding: &Funding,
    wallet: Address,
    amounts: &Amounts,
) -> Result<()> {
    let client = chain::connect_to(network, key, config).await?;
    let deployments = client
        .deployments(file)
        .await
        .with_context(|| format!("no deployment on {key}"))?;
    let funder = fund::funder()?;
    // Through the client's own connection: an endpoint's URL may carry a
    // key, and an error must never name it.
    let provider = client.signing_provider(funder.clone())?;
    let from = funder.address();
    println!("{} ({key}): {wallet}", config.label);

    if amounts.gas {
        let target = funding
            .wallet_coin(key)
            .ok_or_else(|| anyhow!("{key} is not in the funding file"))?;
        let held = client.balance(wallet).await?;
        let need = target.saturating_sub(held);
        if need.is_zero() {
            println!("  coin: holds {}, enough", format_ether(held));
        } else {
            let reserve = funding.reserve(key).unwrap_or(U256::ZERO);
            let available = provider.get_balance(from).await?.saturating_sub(reserve);
            if need > available {
                bail!(
                    "the funder ({from}) can't send {} past its reserve",
                    format_ether(need)
                );
            }
            let hash = send(&provider, from, wallet, need, None).await?;
            println!("  coin: sent {} ({hash})", format_ether(need));
        }
    }

    let usd = deployments.usd;
    let decimals = client.read(usd, ILocalUsd::decimalsCall {}).await?;
    let target = U256::from(amounts.dollars) * U256::from(10u64).pow(U256::from(decimals));
    let held = client
        .read(usd, ILocalUsd::balanceOfCall { account: wallet })
        .await?;
    let need = target.saturating_sub(held);
    if need.is_zero() {
        println!("  dollars: holds {}, enough", units(held, decimals));
    } else {
        let mint = ILocalUsd::mintCall {
            to: wallet,
            amount: need,
        };
        let hash = send(&provider, from, usd, U256::ZERO, Some(mint.abi_encode())).await?;
        println!("  dollars: minted {} ({hash})", units(need, decimals));
    }
    let coin = client.balance(wallet).await?;
    let dollars = client
        .read(usd, ILocalUsd::balanceOfCall { account: wallet })
        .await?;
    println!(
        "  holds {} of the coin and {} dollars",
        format_ether(coin),
        units(dollars, decimals)
    );
    Ok(())
}

async fn solana(
    network: NetworkName,
    key: &ChainKey,
    config: &ChainConfig,
    file: &Path,
    funding: &Funding,
    wallet: Pubkey,
    amounts: &Amounts,
) -> Result<()> {
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("no deployment for {key} at {}", file.display()))?;
    let deployments = chain::SolanaDeployments::from_json(&text)?;
    if deployments.roles.is_none() {
        bail!("{key} is not set up yet: run task chains:solana-setup NETWORK=testnet");
    }
    let client = chain::connect_solana(network, key, config).await?;
    client.check_deployments(&deployments).await?;
    let platform = chain::solana::Platform::from_deployments(&deployments)?;
    println!("{} ({key}): {wallet}", config.label);

    if amounts.gas {
        let target = funding
            .wallet_lamports(key)?
            .ok_or_else(|| anyhow!("{key} is not in the funding file"))?;
        let held = client.lamports(&wallet).await?;
        let need = target.saturating_sub(held);
        if need == 0 {
            println!("  coin: holds {}, enough", sol(held));
        } else {
            let reserve = funding.solana(key)?.map_or(0, |amounts| amounts.reserve);
            let deployer = crate::solana::keypair_from_mnemonic(
                &crate::solana::testnet_mnemonic()?,
                crate::solana::DEPLOYER,
            )?;
            let available = client
                .lamports(&deployer.pubkey())
                .await?
                .saturating_sub(reserve);
            if need > available {
                bail!(
                    "the deployer ({}) can't send {} past its reserve",
                    deployer.pubkey(),
                    sol(need)
                );
            }
            let landed = client
                .send(
                    vec![chain::solana::executor::ix_fund(
                        &deployer.pubkey(),
                        &wallet,
                        need,
                    )],
                    &[&deployer],
                )
                .await
                .context("sending from the deployer")?;
            println!("  coin: sent {} ({})", sol(need), landed.record.signature);
        }
    }

    let decimals = u32::from(chain::solana::programs::vault::state::DECIMALS);
    let target = amounts
        .dollars
        .checked_mul(10u64.pow(decimals))
        .ok_or_else(|| {
            anyhow!(
                "{} dollars is more than a token account holds",
                amounts.dollars
            )
        })?;
    let account = platform.dollars(&wallet);
    let held = client.token_balance(&account).await?;
    let need = target.saturating_sub(held);
    if need == 0 {
        println!("  dollars: holds {}, enough", lamport_units(held, decimals));
    } else {
        let admin = client.role(Role::Admin)?;
        let landed = client
            .send(
                vec![
                    platform.ix_open_dollars(&admin.pubkey(), &wallet),
                    platform.ix_mint_to(&admin.pubkey(), &wallet, need)?,
                ],
                &[admin],
            )
            .await
            .context("minting the stand-in dollar")?;
        println!(
            "  dollars: minted {} into {account} ({})",
            lamport_units(need, decimals),
            landed.record.signature
        );
    }
    println!(
        "  holds {} and {} dollars",
        sol(client.lamports(&wallet).await?),
        lamport_units(client.token_balance(&account).await?, decimals)
    );
    Ok(())
}

/// Send from the funder at a legacy gas price (HyperEVM's public RPC takes
/// nothing else), and wait for it to land.
async fn send<P: Provider>(
    provider: &P,
    from: Address,
    to: Address,
    value: U256,
    input: Option<Vec<u8>>,
) -> Result<String> {
    let price = provider.get_gas_price().await?;
    let mut request = TransactionRequest::default()
        .with_from(from)
        .with_to(to)
        .with_value(value)
        .with_gas_price(price + price / 5 + 1);
    if let Some(input) = input {
        request = request.with_input(input);
    }
    let receipt = provider
        .send_transaction(request)
        .await
        .context("sending from the funder")?
        .with_timeout(Some(Duration::from_secs(120)))
        .get_receipt()
        .await
        .context("waiting for the funder's send to land")?;
    if !receipt.status() {
        bail!("the send to {to} reverted");
    }
    Ok(format!("{:#x}", receipt.transaction_hash))
}

fn units(amount: U256, decimals: u8) -> String {
    alloy::primitives::utils::format_units(amount, decimals).unwrap_or_else(|_| amount.to_string())
}

fn lamport_units(amount: u64, decimals: u32) -> String {
    units(U256::from(amount), decimals as u8)
}

fn sol(lamports: u64) -> String {
    format!("{} SOL", lamport_units(lamports, 9))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_s_form_says_which_chains_it_is_funded_on() {
        assert_eq!(
            Wallet::parse("0x7ed5Ac216DfE8c7F4FdF42Ea0650BACee007f703").unwrap(),
            Wallet::Evm(
                "0x7ed5Ac216DfE8c7F4FdF42Ea0650BACee007f703"
                    .parse()
                    .unwrap()
            )
        );
        assert!(matches!(
            Wallet::parse("72bmoru5cYbTTn3FxAhcJyCfa4AAfiJ3vMnVauKuqfGd").unwrap(),
            Wallet::Solana(_)
        ));
    }

    #[test]
    fn a_malformed_address_is_refused_rather_than_funded() {
        assert!(Wallet::parse("0x1234").is_err());
        assert!(Wallet::parse("not-an-address").is_err());
        assert!(Wallet::parse("").is_err());
    }

    #[test]
    fn whole_dollars_print_at_the_token_s_precision() {
        assert_eq!(lamport_units(5_000_000_000, 6), "5000.000000");
        assert_eq!(sol(100_000_000), "0.100000000 SOL");
    }
}
