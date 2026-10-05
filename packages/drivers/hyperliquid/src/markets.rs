//! The venue's markets, as it numbers them on this network.
//!
//! Testnet and mainnet number perps, spot pairs and tokens differently, and
//! testnet lists look-alike tokens under the same names, so nothing here is a
//! table: every number is read from the venue, and a spot name that two
//! tokens share is refused.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::api::Api;
use crate::payload::Decimal;

/// Spot order assets are the pair's index plus this.
pub const SPOT_ASSET_OFFSET: u32 = 10_000;
/// Decimals a perp price may carry, less the market's size decimals.
const PERP_PRICE_DECIMALS: u32 = 6;
/// The same for spot.
const SPOT_PRICE_DECIMALS: u32 = 8;
/// Significant figures a price may carry, unless it is a whole number.
const PRICE_FIGURES: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Perp { max_leverage: u32 },
    Spot { pair: u32, base_token: u64 },
}

/// One market an agent may name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Market {
    /// The name an agent uses: a perp's coin (`ETH`), or `BASE/USDC`.
    pub name: String,
    /// The number an order names.
    pub asset: u32,
    /// The name the venue's feeds and fills use: a perp's coin, a spot
    /// pair's own name (`PURR/USDC`, or `@<index>`).
    pub coin: String,
    pub sz_decimals: u32,
    pub kind: Kind,
}

impl Market {
    pub fn is_spot(&self) -> bool {
        matches!(self.kind, Kind::Spot { .. })
    }

    /// `price` as the venue's wire writes it, or why the venue would refuse
    /// it: at most five significant figures unless whole, and no more
    /// decimals than the market allows.
    pub fn wire_price(&self, price: Decimal) -> Result<String> {
        let max = if self.is_spot() {
            SPOT_PRICE_DECIMALS
        } else {
            PERP_PRICE_DECIMALS
        }
        .saturating_sub(self.sz_decimals);
        let text = plain(price)?;
        let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
        if fraction.len() as u32 > max {
            bail!(
                "{} takes prices with at most {max} decimals, not {text}",
                self.name
            );
        }
        if !fraction.is_empty() {
            let digits = format!("{whole}{fraction}");
            let figures = digits.trim_start_matches('0').len();
            if figures > PRICE_FIGURES {
                bail!("a price takes at most five significant figures, not {text}");
            }
        }
        Ok(text)
    }

    /// `size` as the wire writes it, or why the venue would refuse it.
    pub fn wire_size(&self, size: Decimal) -> Result<String> {
        let text = plain(size)?;
        let decimals = text.split_once('.').map_or(0, |(_, f)| f.len()) as u32;
        if decimals > self.sz_decimals {
            bail!(
                "{} takes sizes with at most {} decimals, not {text}",
                self.name,
                self.sz_decimals
            );
        }
        Ok(text)
    }
}

/// A positive decimal written plainly, without trailing zeros.
pub fn plain(value: Decimal) -> Result<String> {
    if value <= Decimal::ZERO {
        bail!("{value} is not a positive amount");
    }
    let text = value.to_string();
    if text.contains(['e', 'E']) {
        bail!("{text}: write the amount without an exponent");
    }
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    };
    Ok(text)
}

/// Every market the venue lists for trading, by the name an agent uses.
#[derive(Clone, Debug, Default)]
pub struct Markets {
    by_name: BTreeMap<String, Market>,
    by_coin: BTreeMap<String, String>,
    /// The dollar on the spot market: its wire name for a send.
    pub usdc_wire: String,
}

impl Markets {
    /// Read from the venue.
    pub async fn read(api: &Api) -> Result<Self> {
        let meta = api.info(json!({"type": "meta"})).await?;
        let spot = api.info(json!({"type": "spotMeta"})).await?;
        Self::from_meta(&meta, &spot)
    }

    /// From the venue's `meta` and `spotMeta` answers.
    pub fn from_meta(meta: &Value, spot: &Value) -> Result<Self> {
        let mut markets = Self::default();
        let universe = meta["universe"]
            .as_array()
            .ok_or_else(|| anyhow!("meta has no universe"))?;
        for (index, perp) in universe.iter().enumerate() {
            let Some(name) = perp["name"].as_str() else {
                continue;
            };
            // A delisted perp, or one that trades only isolated, is not one
            // a station's agent may open.
            if perp["isDelisted"].as_bool() == Some(true)
                || perp["onlyIsolated"].as_bool() == Some(true)
            {
                continue;
            }
            markets.insert(Market {
                name: name.to_string(),
                asset: index as u32,
                coin: name.to_string(),
                sz_decimals: perp["szDecimals"].as_u64().unwrap_or(0) as u32,
                kind: Kind::Perp {
                    max_leverage: perp["maxLeverage"].as_u64().unwrap_or(1) as u32,
                },
            });
        }

        let tokens = spot["tokens"]
            .as_array()
            .ok_or_else(|| anyhow!("spotMeta has no tokens"))?;
        let usdc = unique_token(tokens, "USDC")?;
        markets.usdc_wire = format!(
            "USDC:{}",
            usdc["tokenId"]
                .as_str()
                .ok_or_else(|| anyhow!("USDC has no token id"))?
        );
        let usdc_index = usdc["index"].as_u64();
        let pairs = spot["universe"]
            .as_array()
            .ok_or_else(|| anyhow!("spotMeta has no universe"))?;
        for token in tokens {
            let (Some(name), Some(index)) = (token["name"].as_str(), token["index"].as_u64())
            else {
                continue;
            };
            if Some(index) == usdc_index {
                continue;
            }
            // A name two tokens share names neither.
            if tokens.iter().filter(|t| t["name"] == name).count() != 1 {
                continue;
            }
            let Some(pair) = pairs.iter().find(|pair| {
                pair["tokens"][0].as_u64() == Some(index)
                    && pair["tokens"][1].as_u64() == usdc_index
            }) else {
                continue;
            };
            let Some(pair_index) = pair["index"].as_u64() else {
                continue;
            };
            markets.insert(Market {
                name: format!("{name}/USDC"),
                asset: SPOT_ASSET_OFFSET + pair_index as u32,
                coin: pair["name"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("@{pair_index}")),
                sz_decimals: token["szDecimals"].as_u64().unwrap_or(0) as u32,
                kind: Kind::Spot {
                    pair: pair_index as u32,
                    base_token: index,
                },
            });
        }
        Ok(markets)
    }

    fn insert(&mut self, market: Market) {
        self.by_coin
            .insert(market.coin.clone(), market.name.clone());
        self.by_name.insert(market.name.clone(), market);
    }

    pub fn get(&self, name: &str) -> Option<&Market> {
        self.by_name.get(name)
    }

    /// The market a feed or a fill names by the venue's coin.
    pub fn by_coin(&self, coin: &str) -> Option<&Market> {
        self.by_coin
            .get(coin)
            .and_then(|name| self.by_name.get(name))
    }

    /// The perp the venue numbers `index`.
    pub fn perp(&self, index: u32) -> Option<&Market> {
        self.by_name
            .values()
            .find(|m| !m.is_spot() && m.asset == index)
    }

    /// The spot pair the venue numbers `pair`.
    pub fn spot_pair(&self, pair: u32) -> Option<&Market> {
        self.by_name
            .values()
            .find(|m| m.is_spot() && m.asset == SPOT_ASSET_OFFSET + pair)
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

fn unique_token<'a>(tokens: &'a [Value], name: &str) -> Result<&'a Value> {
    let mut found = tokens.iter().filter(|t| t["name"] == name);
    match (found.next(), found.next()) {
        (Some(token), None) => Ok(token),
        (None, _) => bail!("the venue lists no token named {name}"),
        _ => bail!("the venue lists more than one token named {name}"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn sample() -> Markets {
        let meta = json!({"universe": [
            {"name": "SOL", "szDecimals": 2, "maxLeverage": 20},
            {"name": "OLD", "szDecimals": 2, "maxLeverage": 3, "isDelisted": true},
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
            {"name": "ISO", "szDecimals": 1, "maxLeverage": 3, "onlyIsolated": true},
            {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
        ]});
        let spot = json!({
            "tokens": [
                {"name": "USDC", "index": 0, "szDecimals": 8, "weiDecimals": 8, "tokenId": "0xaa"},
                {"name": "HYPE", "index": 1105, "szDecimals": 2, "weiDecimals": 8, "tokenId": "0xbb"},
                {"name": "FAKE", "index": 7, "szDecimals": 0, "weiDecimals": 5, "tokenId": "0xcc"},
                {"name": "FAKE", "index": 8, "szDecimals": 0, "weiDecimals": 5, "tokenId": "0xdd"}
            ],
            "universe": [
                {"name": "@1035", "tokens": [1105, 0], "index": 1035},
                {"name": "@3", "tokens": [7, 0], "index": 3}
            ]
        });
        Markets::from_meta(&meta, &spot).unwrap()
    }

    fn d(text: &str) -> Decimal {
        text.parse().unwrap()
    }

    #[test]
    fn asset_numbers_come_from_the_venues_own_lists() {
        let markets = sample();
        assert_eq!(markets.get("ETH").unwrap().asset, 4);
        assert_eq!(markets.get("HYPE/USDC").unwrap().asset, 11_035);
        assert_eq!(markets.by_coin("@1035").unwrap().name, "HYPE/USDC");
        assert_eq!(markets.perp(2).unwrap().name, "BTC");
        assert_eq!(markets.spot_pair(1035).unwrap().name, "HYPE/USDC");
        assert_eq!(markets.usdc_wire, "USDC:0xaa");
    }

    #[test]
    fn delisted_isolated_only_and_look_alike_markets_are_not_listed() {
        let markets = sample();
        assert!(markets.get("OLD").is_none());
        assert!(markets.get("ISO").is_none());
        assert!(markets.get("FAKE/USDC").is_none());
    }

    #[test]
    fn prices_keep_to_five_figures_and_the_markets_decimals() {
        let markets = sample();
        let eth = markets.get("ETH").unwrap();
        assert_eq!(eth.wire_price(d("2673.50")).unwrap(), "2673.5");
        assert_eq!(eth.wire_price(d("123456")).unwrap(), "123456");
        assert!(eth.wire_price(d("2673.51")).is_err(), "six figures");
        assert!(
            eth.wire_price(d("0.001")).is_err(),
            "three decimals on a four-decimal perp"
        );
        assert!(eth.wire_price(d("0")).is_err());
        let hype = markets.get("HYPE/USDC").unwrap();
        assert_eq!(hype.wire_price(d("22.513")).unwrap(), "22.513");
        assert_eq!(hype.wire_size(d("1.50")).unwrap(), "1.5");
        assert!(hype.wire_size(d("1.555")).is_err());
    }
}
