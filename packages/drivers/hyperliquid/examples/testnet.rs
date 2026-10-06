//! Hand tools for Hyperliquid testnet, for showing a station trade: bring an
//! account into being, move a role onto big blocks, send it HYPE, trade a
//! station's account with its agent key, and read an account.
//!
//! Testnet only. Keys come from `SHONIN_TESTNET_EVM_MNEMONIC`
//! (`m/44'/60'/0'/0/<index>`); the funder is index 100.
//!
//! ```text
//! cargo run -p shonin-hyperliquid --example testnet -- <command> ...
//!   address <index>
//!   activate <address> <dollars>          the funder sends dollars to an account
//!   big-blocks <index> <on|off>
//!   send-hype <address> <amount>          the funder sends HYPE on HyperCore
//!   order <agent-index> <station> <market> <buy|sell> <size> <price> <ioc|gtc> [reduce]
//!   move <agent-index> <station> <perp|spot> <dollars>   between the account's own balances
//!   events <address> <seconds>          print the account's events
//!   state <address>
//! ```

use std::collections::BTreeSet;

use alloy::primitives::Address;
use alloy::signers::local::{MnemonicBuilder, PrivateKeySigner, coins_bip39::English};
use anyhow::{Context, Result, anyhow, bail};
use shonin_hyperliquid::agent::{Allowlist, Trader};
use shonin_hyperliquid::payload::{Action, Balance, Order, Side, Tif};
use shonin_hyperliquid::{Api, HyperCore, Markets, Network};

const FUNDER: u32 = 100;

fn key(index: u32) -> Result<PrivateKeySigner> {
    let phrase = std::env::var("SHONIN_TESTNET_EVM_MNEMONIC")
        .context("SHONIN_TESTNET_EVM_MNEMONIC is not set")?;
    Ok(MnemonicBuilder::<English>::default()
        .phrase(phrase.trim())
        .index(index)?
        .build()?)
}

fn arg(args: &[String], at: usize) -> Result<&str> {
    args.get(at)
        .map(String::as_str)
        .ok_or_else(|| anyhow!("missing argument {at}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let api = Api::new(Network::Testnet);
    match arg(&args, 0)? {
        "address" => println!("{:?}", key(arg(&args, 1)?.parse()?)?.address()),
        "activate" => {
            let to: Address = arg(&args, 1)?.parse()?;
            let dollars: f64 = arg(&args, 2)?.parse()?;
            HyperCore::new(api, key(FUNDER)?)
                .usd_send(to, dollars)
                .await?;
            println!("sent ${dollars} to {to:?} on HyperCore");
        }
        "big-blocks" => {
            let signer = key(arg(&args, 1)?.parse()?)?;
            let on = arg(&args, 2)? == "on";
            HyperCore::new(api, signer).use_big_blocks(on).await?;
            println!("big blocks {on}");
        }
        "send-hype" => {
            let to: Address = arg(&args, 1)?.parse()?;
            let amount: f64 = arg(&args, 2)?.parse()?;
            let hype = api.spot_token("HYPE").await?;
            HyperCore::new(api, key(FUNDER)?)
                .spot_send(to, &hype, amount)
                .await?;
            println!("sent {amount} HYPE to {to:?} on HyperCore");
        }
        "order" => {
            let agent = key(arg(&args, 1)?.parse()?)?;
            let station: Address = arg(&args, 2)?.parse()?;
            let market = arg(&args, 3)?.to_string();
            let side = match arg(&args, 4)? {
                "buy" => Side::Buy,
                "sell" => Side::Sell,
                other => bail!("{other} is not buy or sell"),
            };
            let tif = match arg(&args, 7)? {
                "ioc" => Tif::Ioc,
                "gtc" => Tif::Gtc,
                other => bail!("{other} is not ioc or gtc"),
            };
            let markets = Markets::read(&api).await?;
            let allowlist = Allowlist {
                master: station,
                markets,
                allowed: BTreeSet::from([market.clone()]),
                missing: Default::default(),
            };
            let trader = Trader::new(api, agent, allowlist);
            let receipt = trader
                .submit(&Action::Place(Order {
                    market,
                    side,
                    size: arg(&args, 5)?.parse().map_err(|e| anyhow!("{e:?}"))?,
                    limit_price: arg(&args, 6)?.parse().map_err(|e| anyhow!("{e:?}"))?,
                    tif,
                    reduce_only: args.get(8).is_some_and(|a| a == "reduce"),
                    client_id: None,
                }))
                .await?;
            println!("{receipt:?}");
        }
        "move" => {
            let agent = key(arg(&args, 1)?.parse()?)?;
            let station: Address = arg(&args, 2)?.parse()?;
            let from = match arg(&args, 3)? {
                "perp" => Balance::Perp,
                "spot" => Balance::Spot,
                other => bail!("{other} is not perp or spot"),
            };
            let amount = arg(&args, 4)?.parse().map_err(|e| anyhow!("{e:?}"))?;
            let allowlist = Allowlist {
                master: station,
                markets: Markets::read(&api).await?,
                allowed: BTreeSet::new(),
                missing: Default::default(),
            };
            let receipt = Trader::new(api, agent, allowlist)
                .submit(&Action::MoveDollars { from, amount })
                .await?;
            println!("{receipt:?}");
        }
        "events" => {
            let who: Address = arg(&args, 1)?.parse()?;
            let seconds: u64 = arg(&args, 2)?.parse()?;
            let markets = Markets::read(&api).await?;
            let mut events = shonin_hyperliquid::events::account_events(api.network, who, markets);
            let until = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
            while let Ok(Some(event)) = tokio::time::timeout_at(until, events.recv()).await {
                println!("{event:?}");
            }
        }
        "state" => {
            let who: Address = arg(&args, 1)?.parse()?;
            let markets = Markets::read(&api).await?;
            let state = shonin_hyperliquid::events::account_state(&api, &markets, who).await?;
            println!("{state:#?}");
        }
        other => bail!("unknown command {other}"),
    }
    Ok(())
}
