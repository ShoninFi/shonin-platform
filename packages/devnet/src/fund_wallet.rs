//! Funding a person's own wallet on the testnets, so the web app's money
//! flows can be tried with it: the chain's dollar where anyone may mint it,
//! and with `--gas` the chain's coin.
//!
//! Each testnet settles in its own public dollar. Where that dollar's mint is
//! open to anyone (Paxos's USDG on the Robinhood Chain testnet), the funder
//! (which holds the gas) sends the mint; elsewhere (Circle's USDC) the
//! dollar comes from its issuer's faucet alone, and none is minted. The coin
//! comes from each chain's funder (the EVM funder, the Solana deployer),
//! never past the reserve `config/funding.toml` keeps for it.
//!
//! On Hyperliquid's testnet (HyperEVM) the funder's money is on HyperCore:
//! the dollars (Circle's USDC, which can't be minted) are sent home from its
//! HyperCore balance to the wallet on HyperEVM, and what its HyperEVM HYPE
//! above the reserve can't cover of the coin is bought on HyperCore's spot
//! market (at no more than the funding file's price) and brought across
//! first, as `chains:fund` does for the roles. Both are
//! top-ups: a wallet already holding its amount is left alone, so running it
//! again sends nothing new. Testnet only.

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
use chain::{ChainConfig, ChainKey, Deployments, NetworkName};

use crate::fund::{self, Funding};
use crate::hyperliquid_vaults;

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
    /// Whole dollars of the chain's dollar, where it may be minted.
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

    let hyperliquid = client.chain_id == hyperliquid_vaults::CHAIN_ID;

    if amounts.gas {
        let target = funding
            .wallet_coin(key)
            .ok_or_else(|| anyhow!("{key} is not in the funding file"))?;
        let held = client.balance(wallet).await?;
        let need = target.saturating_sub(held);
        let reserve = funding.reserve(key).unwrap_or(U256::ZERO);
        let surplus = || async {
            Ok::<_, anyhow::Error>(provider.get_balance(from).await?.saturating_sub(reserve))
        };
        match gas_source(need, surplus().await?, hyperliquid) {
            GasSource::Enough => println!("  coin: holds {}, enough", format_ether(held)),
            GasSource::Funder => {
                let hash = send(&provider, from, wallet, need, None).await?;
                println!("  coin: sent {} ({hash})", format_ether(need));
            }
            GasSource::HyperCore { missing } => {
                // What the funder's HyperEVM HYPE above its reserve lacks
                // comes across from HyperCore first, bought there at no
                // more than the funding file's price.
                let api = hyperliquid::Api::for_chain(client.chain_id)
                    .ok_or_else(|| anyhow!("{key} has no HyperCore"))?;
                let core = hyperliquid::HyperCore::new(api.clone(), funder.clone());
                fund::hype_from_hypercore(
                    funding,
                    &core,
                    &api,
                    &provider,
                    from,
                    format_ether(missing).parse()?,
                    true,
                )
                .await
                .context("bringing HYPE across from HyperCore")?;
                if need > surplus().await? {
                    bail!(
                        "the funder ({from}) can't send {} past its reserve, even with HYPE from HyperCore",
                        format_ether(need)
                    );
                }
                let hash = send(&provider, from, wallet, need, None).await?;
                println!(
                    "  coin: sent {} ({hash}), with HYPE from the funder's HyperCore balance",
                    format_ether(need)
                );
            }
            GasSource::Short => bail!(
                "the funder ({from}) can't send {} past its reserve",
                format_ether(need)
            ),
        }
    }

    let usd = deployments.usd;
    if hyperliquid {
        // HyperEVM's USDC can't be minted: it is sent home from the
        // funder's HyperCore balance.
        if usd != hyperliquid_vaults::USDC {
            bail!(
                "the deployment's dollar ({usd}) is not HyperEVM's USDC ({}), the dollar HyperCore sends home",
                hyperliquid_vaults::USDC
            );
        }
        let decimals = client.read(usd, ILocalUsd::decimalsCall {}).await?;
        let held = client
            .read(usd, ILocalUsd::balanceOfCall { account: wallet })
            .await?;
        let need = dollars_to_send(amounts.dollars, decimals, held);
        if need.is_zero() {
            println!("  dollars: holds {}, enough", units(held, decimals));
        } else {
            let sent = hyperliquid_vaults::Funder::new(&client)?
                .send_dollars(&client, wallet, need)
                .await?;
            if let Some(dollars) = sent.from_core {
                println!(
                    "  dollars: the funder sent {dollars} USDC home from HyperCore to HyperEVM"
                );
            }
            println!("  dollars: sent {} ({})", units(need, decimals), sent.hash);
        }
        return holdings(&client, usd, wallet, decimals).await;
    }
    // Minted only where the chain's public dollar is open to anyone (the
    // Robinhood Chain testnet's USDG).
    let mintable = crate::deploy::public_dollar(client.chain_id)
        .is_some_and(|dollar| dollar.mintable && dollar.address == usd);
    if !mintable {
        println!(
            "  dollars: this chain's dollar ({usd}) is nobody's to mint here: its issuer's faucet hands it out"
        );
        return Ok(());
    }
    let decimals = client.read(usd, ILocalUsd::decimalsCall {}).await?;
    let held = client
        .read(usd, ILocalUsd::balanceOfCall { account: wallet })
        .await?;
    let need = dollars_to_send(amounts.dollars, decimals, held);
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
    holdings(&client, usd, wallet, decimals).await
}

/// Print what the wallet holds of the coin and the dollar.
async fn holdings(
    client: &chain::Client,
    usd: Address,
    wallet: Address,
    decimals: u8,
) -> Result<()> {
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

/// What tops a wallet holding `held` (in the dollar's units) up to `dollars`
/// whole dollars: nothing if it holds that already.
fn dollars_to_send(dollars: u64, decimals: u8, held: U256) -> U256 {
    let target = U256::from(dollars) * U256::from(10u64).pow(U256::from(decimals));
    target.saturating_sub(held)
}

/// Where a wallet's coin comes from.
#[derive(Debug, PartialEq, Eq)]
enum GasSource {
    /// Nowhere: it holds its amount.
    Enough,
    /// The funder's coin above its reserve on this chain.
    Funder,
    /// The same, after `missing` (what that surplus lacks) is brought across
    /// from HyperCore.
    HyperCore { missing: U256 },
    /// Nowhere: the funder's surplus is short, and there is no HyperCore to
    /// make it up.
    Short,
}

/// Where a wallet short `need` of its coin is topped up from, given what the
/// funder holds above its reserve (`surplus`) and whether the chain is
/// HyperEVM, whose funder can bring HYPE across from HyperCore.
fn gas_source(need: U256, surplus: U256, hyperliquid: bool) -> GasSource {
    if need.is_zero() {
        GasSource::Enough
    } else if need <= surplus {
        GasSource::Funder
    } else if hyperliquid {
        GasSource::HyperCore {
            missing: need - surplus,
        }
    } else {
        GasSource::Short
    }
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
        // A public cluster's dollar (Circle's USDC on devnet): nobody here
        // mints it.
        println!(
            "  dollars: holds {}; the dollar ({}) comes from its issuer's faucet alone, so none is minted",
            lamport_units(held, decimals),
            platform.mint
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
    fn dollars_top_a_wallet_up_to_its_amount_and_no_further() {
        let dollar = U256::from(1_000_000u64);
        assert_eq!(dollars_to_send(25, 6, U256::ZERO), dollar * U256::from(25));
        assert_eq!(
            dollars_to_send(25, 6, dollar * U256::from(10)),
            dollar * U256::from(15)
        );
        assert_eq!(dollars_to_send(25, 6, dollar * U256::from(25)), U256::ZERO);
        assert_eq!(dollars_to_send(25, 6, dollar * U256::from(40)), U256::ZERO);
        assert_eq!(dollars_to_send(0, 6, U256::ZERO), U256::ZERO);
        assert_eq!(
            dollars_to_send(1, 18, U256::ZERO),
            U256::from(10u64).pow(U256::from(18))
        );
    }

    #[test]
    fn a_wallet_holding_its_coin_is_sent_none_from_anywhere() {
        let surplus = U256::from(5u64);
        assert_eq!(gas_source(U256::ZERO, surplus, true), GasSource::Enough);
        assert_eq!(gas_source(U256::ZERO, U256::ZERO, false), GasSource::Enough);
    }

    #[test]
    fn the_coin_comes_from_the_funder_s_surplus_when_it_covers_the_need() {
        assert_eq!(
            gas_source(U256::from(3u64), U256::from(5u64), true),
            GasSource::Funder
        );
        assert_eq!(
            gas_source(U256::from(5u64), U256::from(5u64), false),
            GasSource::Funder
        );
    }

    #[test]
    fn on_hyperevm_hypercore_makes_up_only_what_the_surplus_lacks() {
        assert_eq!(
            gas_source(U256::from(8u64), U256::from(5u64), true),
            GasSource::HyperCore {
                missing: U256::from(3u64)
            }
        );
        assert_eq!(
            gas_source(U256::from(8u64), U256::ZERO, true),
            GasSource::HyperCore {
                missing: U256::from(8u64)
            }
        );
    }

    #[test]
    fn elsewhere_a_short_funder_is_refused_rather_than_dipping_into_its_reserve() {
        assert_eq!(
            gas_source(U256::from(8u64), U256::from(5u64), false),
            GasSource::Short
        );
    }

    #[test]
    fn whole_dollars_print_at_the_token_s_precision() {
        assert_eq!(lamport_units(5_000_000_000, 6), "5000.000000");
        assert_eq!(sol(100_000_000), "0.100000000 SOL");
    }
}
