//! The Hyperliquid driver's executor half, as this executor runs it.
//!
//! The vault's station on HyperEVM is the master of its HyperCore account
//! and has approved this vault's own key as its named agent (the key is made
//! per vault, and the venue lets one agent address serve one account only).
//! Everything a strategy submits through the `hyperliquid` driver is decoded
//! and checked against the allowlist in `hyperliquid::agent`, for the markets
//! the station's pinned driver lists, before that key signs it.

use std::sync::Arc;

use alloy::primitives::{Address, B256};
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, anyhow};
use chain::Client;
use hyperliquid::agent::{Allowlist, Outcome, Trader};
use hyperliquid::{Api, Markets};
use tokio::sync::mpsc::UnboundedSender;

/// The name `shonin.toml` writes for it.
pub const DRIVER: &str = "hyperliquid";

alloy::sol! {
    interface IPinnedStation {
        function pinnedDriver(bytes32 driver) external view returns (address);
        function value() external view returns (uint256 mark, uint256 exit);
    }
    interface IVaultCash {
        function cash() external view returns (uint256);
    }
    interface IHyperliquidDriver {
        struct Perp { uint32 index; uint8 szDecimals; }
        struct Spot { uint32 pair; uint64 token; uint8 szDecimals; uint8 weiDecimals; }
        function perps() external view returns (Perp[] memory);
        function spots() external view returns (Spot[] memory);
    }
}

/// The driver's name as a station keys it: the name, zero-padded.
pub fn driver_name() -> B256 {
    let mut name = [0u8; 32];
    name[..DRIVER.len()].copy_from_slice(DRIVER.as_bytes());
    B256::from(name)
}

/// One vault's Hyperliquid account, and the key that trades it.
pub struct Half {
    pub trader: Trader,
    api: Api,
    chain: Client,
    vault: Address,
    station: Address,
}

impl Half {
    /// Read what the station's pinned driver lists, and the venue's numbers
    /// for it, and hold `agent` to them.
    pub async fn connect(
        chain: &Client,
        vault: Address,
        station: Address,
        agent: PrivateKeySigner,
    ) -> Result<Arc<Self>> {
        let api = Api::for_chain(chain.chain_id)
            .ok_or_else(|| anyhow!("chain {} is not HyperEVM", chain.chain_id))?;
        let driver = chain
            .read(
                station,
                IPinnedStation::pinnedDriverCall {
                    driver: driver_name(),
                },
            )
            .await
            .context("reading the station's pinned Hyperliquid driver")?;
        let perps: Vec<u32> = chain
            .read(driver, IHyperliquidDriver::perpsCall {})
            .await?
            .into_iter()
            .map(|perp| perp.index)
            .collect();
        let spots: Vec<u32> = chain
            .read(driver, IHyperliquidDriver::spotsCall {})
            .await?
            .into_iter()
            .map(|spot| spot.pair)
            .collect();
        let markets = Markets::read(&api).await?;
        let allowlist = Allowlist::from_driver(station, markets, &perps, &spots)?;
        Ok(Arc::new(Self {
            trader: Trader::new(api.clone(), agent, allowlist),
            api,
            chain: chain.clone(),
            vault,
            station,
        }))
    }

    pub async fn submit(&self, payload: &[u8]) -> Outcome<Vec<u8>> {
        self.trader.submit_payload(payload).await
    }

    /// The account now, as a payload.
    pub async fn state(&self) -> Result<Vec<u8>> {
        let mut state = hyperliquid::events::account_state(
            &self.api,
            &self.trader.allowlist.markets,
            self.station,
        )
        .await?;
        let cash = self.chain.read(self.vault, IVaultCash::cashCall {}).await?;
        let held = self
            .chain
            .read(self.station, IPinnedStation::valueCall {})
            .await?
            .mark;
        state.idle = dollars(cash);
        state.vault_value = dollars(cash + held);
        Ok(hyperliquid::agent::encode(&state))
    }

    /// Hand the account's events to the vault's feed, as payloads, until
    /// the task is dropped.
    pub fn forward_events(&self, sender: UnboundedSender<Vec<u8>>) -> tokio::task::JoinHandle<()> {
        let mut events = hyperliquid::events::account_events(
            self.api.network,
            self.station,
            self.trader.allowlist.markets.clone(),
        );
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if sender.send(hyperliquid::agent::encode(&event)).is_err() {
                    return;
                }
            }
        })
    }
}

/// Until when, in seconds, the venue takes `agent` as the station's named
/// agent: zero if it doesn't (never approved, replaced, or an approval the
/// venue dropped because the account didn't exist yet). What the station's
/// approval is compared with, as the stand-in venue's own record is.
pub async fn agent_valid_until(chain_id: u64, station: Address, agent: Address) -> Result<u64> {
    let api =
        Api::for_chain(chain_id).ok_or_else(|| anyhow!("chain {chain_id} is not HyperEVM"))?;
    let agents = api
        .info(serde_json::json!({"type": "extraAgents", "user": format!("{station:#x}")}))
        .await
        .context("reading the station's agents from the venue")?;
    Ok(valid_until_of(&agents, agent))
}

/// [`agent_valid_until`], from the venue's answer: milliseconds there.
fn valid_until_of(agents: &serde_json::Value, agent: Address) -> u64 {
    agents
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry["address"]
                .as_str()
                .and_then(|address| address.parse::<Address>().ok())
                == Some(agent)
        })
        .filter_map(|entry| entry["validUntil"].as_u64())
        .map(|ms| ms / 1000)
        .max()
        .unwrap_or(0)
}

/// Millionths of a dollar, as a decimal number of dollars.
fn dollars(millionths: alloy::primitives::U256) -> hyperliquid::payload::Decimal {
    let units = u64::try_from(millionths).unwrap_or(u64::MAX);
    hyperliquid::payload::Decimal::from(units) / hyperliquid::payload::Decimal::from(1_000_000u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_driver_is_named_as_the_station_keys_it() {
        let name = driver_name();
        assert_eq!(&name[..11], b"hyperliquid");
        assert!(name[11..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn an_agent_s_validity_is_read_from_the_venue_in_seconds_and_zero_when_absent() {
        let agent: Address = "0x369f7e7819454c8a5c62e8ee1f73f1b2f94844dc"
            .parse()
            .unwrap();
        let answer = serde_json::json!([
            {"name": "shonin", "address": "0x369f7e7819454c8a5c62e8ee1f73f1b2f94844dc", "validUntil": 1791498294000u64},
            {"name": "other", "address": "0x1111111111111111111111111111111111111111", "validUntil": 1u64},
        ]);
        assert_eq!(valid_until_of(&answer, agent), 1_791_498_294);
        assert_eq!(valid_until_of(&answer, Address::repeat_byte(2)), 0);
        assert_eq!(valid_until_of(&serde_json::json!([]), agent), 0);
    }

    #[test]
    fn millionths_read_as_dollars() {
        let value = dollars(alloy::primitives::U256::from(20_403_787u64));
        assert_eq!(value, "20.403787".parse().unwrap());
    }
}
