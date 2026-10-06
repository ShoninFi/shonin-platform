//! The driver's executor half: what the executor signs for a vault's Safe.
//!
//! An agent's [`Action`] is decoded here, inside the measured image, and
//! turned into the only calls the executor's key can make: through the
//! station's Roles module, an approval of the router for the token being
//! spent, and one Uniswap swap of a listed stock token against the dollar,
//! paid to the Safe. Before anything is signed, the swap's minimum is checked
//! against the token's Chainlink price and the station's exit band (the same
//! check the role makes on-chain), and refused if the feed is too old, the
//! token's oracle is paused, or the minimum is further from the feed than
//! the band.

use std::collections::BTreeMap;

use alloy::primitives::{Address, Bytes, U256, aliases::U24};
use alloy::providers::DynProvider;
use alloy::sol_types::SolCall;
use anyhow::{Result, anyhow, bail};

use crate::contracts::{
    IAggregatorV3, IRobinhoodDriver, IRoles, IStationSpec, IStockToken, ISwapRouter02, ROLE,
};
use crate::payload::{Action, Decimal};

const BPS: u64 = 10_000;

/// One listed stock token, as the driver lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub symbol: String,
    pub token: Address,
    pub feed: Address,
    pub pool: Address,
    pub fee: u32,
    pub token_decimals: u8,
    pub feed_decimals: u8,
}

/// A call the executor's key makes: to the station's Roles module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleCall {
    pub to: Address,
    pub call: IRoles::execTransactionWithRoleCall,
}

impl RoleCall {
    /// The call's encoded data.
    pub fn data(&self) -> Bytes {
        self.call.abi_encode().into()
    }
}

/// A feed's latest round, as read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedRound {
    /// In the feed's own decimals.
    pub answer: U256,
    pub updated_at: u64,
}

/// What a swap's minimum is checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub round: FeedRound,
    pub paused: bool,
    pub now: u64,
    pub max_feed_age: u64,
    pub band_bps: u16,
}

/// The executor half for one station.
#[derive(Clone, Debug)]
pub struct Trader {
    pub driver: Address,
    pub station: Address,
    pub safe: Address,
    pub roles: Address,
    pub router: Address,
    pub dollar: Address,
    pub dollar_decimals: u8,
    pub max_feed_age: u64,
    pub band_bps: u16,
    pub listed: BTreeMap<String, Listed>,
}

impl Trader {
    /// Read everything the half needs from the driver and the station.
    pub async fn load(provider: &DynProvider, driver: Address, station: Address) -> Result<Self> {
        let d = IRobinhoodDriver::new(driver, provider);
        let safe = d.safeOf(station).call().await?;
        let roles = d.rolesOf(station).call().await?;
        let router = d.router().call().await?;
        let dollar = d.dollar().call().await?;
        let max_feed_age = u64::try_from(d.maxFeedAge().call().await?).unwrap_or(u64::MAX);
        let band_bps = IStationSpec::new(station, provider)
            .spec()
            .call()
            .await?
            .exitBandBps;
        let dollar_decimals = IStockToken::new(dollar, provider).decimals().call().await?;
        let mut listed = BTreeMap::new();
        for stock in d.stocks().call().await? {
            let token = IStockToken::new(stock.token, provider);
            let symbol = token.symbol().call().await?;
            let token_decimals = token.decimals().call().await?;
            let feed_decimals = IAggregatorV3::new(stock.feed, provider)
                .decimals()
                .call()
                .await?;
            listed.insert(
                symbol.clone(),
                Listed {
                    symbol,
                    token: stock.token,
                    feed: stock.feed,
                    pool: stock.pool,
                    fee: stock.fee.to::<u32>(),
                    token_decimals,
                    feed_decimals,
                },
            );
        }
        Ok(Self {
            driver,
            station,
            safe,
            roles,
            router,
            dollar,
            dollar_decimals,
            max_feed_age,
            band_bps,
            listed,
        })
    }

    /// Read what a swap of `token` is checked against now.
    pub async fn bounds(&self, provider: &DynProvider, token: &str) -> Result<Bounds> {
        let listed = self.listing(token)?;
        let round = IAggregatorV3::new(listed.feed, provider)
            .latestRoundData()
            .call()
            .await?;
        if round.answer.is_negative() || round.answer.is_zero() {
            bail!("the {token} feed's price is not positive");
        }
        // A token without the call (the testnet's) has nothing to pause.
        let paused = IStockToken::new(listed.token, provider)
            .oraclePaused()
            .call()
            .await
            .unwrap_or(false);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        Ok(Bounds {
            round: FeedRound {
                answer: round.answer.into_raw(),
                updated_at: u64::try_from(round.updatedAt).unwrap_or(u64::MAX),
            },
            paused,
            now,
            max_feed_age: self.max_feed_age,
            band_bps: self.band_bps,
        })
    }

    fn listing(&self, token: &str) -> Result<&Listed> {
        self.listed
            .get(token)
            .ok_or_else(|| anyhow!("{token} is not a stock token this driver lists"))
    }

    /// The role calls for `action`, or why it is refused.
    pub fn prepare(&self, action: &Action, bounds: &Bounds) -> Result<Vec<RoleCall>> {
        let (token, selling, amount_in, min_out) = match action {
            Action::Buy {
                token,
                amount,
                min_tokens,
            } => (token, false, amount, min_tokens),
            Action::Sell {
                token,
                tokens,
                min_amount,
            } => (token, true, tokens, min_amount),
        };
        let listed = self.listing(token)?;
        if bounds.paused {
            bail!("{token}'s oracle is paused by its issuer");
        }
        if bounds.round.updated_at > bounds.now
            || bounds.now - bounds.round.updated_at > bounds.max_feed_age
        {
            bail!("{token}'s feed is older than the driver trusts");
        }
        let (in_decimals, out_decimals) = if selling {
            (listed.token_decimals, self.dollar_decimals)
        } else {
            (self.dollar_decimals, listed.token_decimals)
        };
        let amount_in = to_units(*amount_in, in_decimals)?;
        let min_out = to_units(*min_out, out_decimals)?;
        if amount_in.is_zero() {
            bail!("nothing to swap");
        }
        let fair = fair_out(
            amount_in,
            selling,
            bounds.round.answer,
            listed.token_decimals + listed.feed_decimals - self.dollar_decimals,
        );
        let floor = ceil_div(
            fair * U256::from(BPS - u64::from(bounds.band_bps.min(10_000))),
            U256::from(BPS),
        );
        if min_out < floor {
            bail!(
                "the minimum is further from {token}'s feed than the band allows ({min_out} < {floor})"
            );
        }
        let (token_in, token_out) = if selling {
            (listed.token, self.dollar)
        } else {
            (self.dollar, listed.token)
        };
        let approve = IStockToken::approveCall {
            spender: self.router,
            amount: amount_in,
        }
        .abi_encode();
        let swap = ISwapRouter02::exactInputSingleCall {
            params: ISwapRouter02::ExactInputSingleParams {
                tokenIn: token_in,
                tokenOut: token_out,
                fee: U24::from(listed.fee),
                recipient: self.safe,
                amountIn: amount_in,
                amountOutMinimum: min_out,
                sqrtPriceLimitX96: Default::default(),
            },
        }
        .abi_encode();
        Ok(vec![
            self.through_role(token_in, approve),
            self.through_role(self.router, swap),
        ])
    }

    fn through_role(&self, to: Address, data: Vec<u8>) -> RoleCall {
        RoleCall {
            to: self.roles,
            call: IRoles::execTransactionWithRoleCall {
                to,
                value: U256::ZERO,
                data: data.into(),
                operation: 0,
                roleKey: ROLE.into(),
                shouldRevert: true,
            },
        }
    }
}

/// What `amount_in` is worth on the other side at the feed's price: dollars
/// for tokens sold, tokens for dollars spent. `scale_digits` is token
/// decimals plus feed decimals less dollar decimals.
pub fn fair_out(amount_in: U256, selling: bool, price: U256, scale_digits: u8) -> U256 {
    let scale = U256::from(10u64).pow(U256::from(scale_digits));
    if selling {
        amount_in * price / scale
    } else {
        amount_in * scale / price
    }
}

fn ceil_div(a: U256, b: U256) -> U256 {
    if a.is_zero() {
        U256::ZERO
    } else {
        (a - U256::from(1u8)) / b + U256::from(1u8)
    }
}

/// A decimal amount in a token's smallest units, rounded down; refused if
/// negative or too large.
pub fn to_units(amount: Decimal, decimals: u8) -> Result<U256> {
    if amount.is_sign_negative() {
        bail!("a negative amount");
    }
    let scaled = (amount * Decimal::from(10u64).powi(i32::from(decimals))).floor();
    let text = scaled.to_string();
    let digits = text.split('.').next().unwrap_or("0");
    U256::from_str_radix(digits, 10).map_err(|_| anyhow!("amount {amount} is out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trader() -> Trader {
        let mut listed = BTreeMap::new();
        listed.insert(
            "NVDA".to_string(),
            Listed {
                symbol: "NVDA".into(),
                token: Address::repeat_byte(0x11),
                feed: Address::repeat_byte(0x12),
                pool: Address::repeat_byte(0x13),
                fee: 500,
                token_decimals: 18,
                feed_decimals: 8,
            },
        );
        Trader {
            driver: Address::repeat_byte(1),
            station: Address::repeat_byte(2),
            safe: Address::repeat_byte(3),
            roles: Address::repeat_byte(4),
            router: Address::repeat_byte(5),
            dollar: Address::repeat_byte(6),
            dollar_decimals: 6,
            max_feed_age: 4 * 86_400,
            band_bps: 500,
            listed,
        }
    }

    /// NVDA at $200, updated an hour ago.
    fn bounds() -> Bounds {
        Bounds {
            round: FeedRound {
                answer: U256::from(200u64 * 100_000_000),
                updated_at: 1_000_000 - 3_600,
            },
            paused: false,
            now: 1_000_000,
            max_feed_age: 4 * 86_400,
            band_bps: 500,
        }
    }

    fn d(text: &str) -> Decimal {
        text.parse().unwrap()
    }

    #[test]
    fn a_buy_within_the_band_becomes_an_approval_and_a_swap_through_the_role_paid_to_the_safe() {
        let t = trader();
        let calls = t
            .prepare(
                &Action::Buy {
                    token: "NVDA".into(),
                    amount: d("1000"),
                    min_tokens: d("4.8"),
                },
                &bounds(),
            )
            .unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|call| call.to == t.roles));
        let outer = calls[1].call.clone();
        assert_eq!(outer.to, t.router);
        assert_eq!(outer.roleKey, alloy::primitives::B256::from(ROLE));
        let swap = ISwapRouter02::exactInputSingleCall::abi_decode(&outer.data).unwrap();
        assert_eq!(swap.params.recipient, t.safe, "always paid to the Safe");
        assert_eq!(swap.params.amountIn, U256::from(1_000_000_000u64));
        assert_eq!(
            swap.params.amountOutMinimum,
            U256::from(48u64) * U256::from(10u64).pow(U256::from(17u8))
        );
        let outer = calls[0].call.clone();
        assert_eq!(
            outer.to, t.dollar,
            "the approval is of the dollar being spent"
        );
    }

    #[test]
    fn a_minimum_further_from_the_feed_than_the_band_is_refused() {
        // Five shares are worth $1,000; the band allows 4.75.
        let refused = trader().prepare(
            &Action::Buy {
                token: "NVDA".into(),
                amount: d("1000"),
                min_tokens: d("4.74"),
            },
            &bounds(),
        );
        assert!(refused.is_err());
        let refused = trader().prepare(
            &Action::Sell {
                token: "NVDA".into(),
                tokens: d("5"),
                min_amount: d("0"),
            },
            &bounds(),
        );
        assert!(refused.is_err(), "a sale with no minimum is refused");
        let accepted = trader().prepare(
            &Action::Sell {
                token: "NVDA".into(),
                tokens: d("5"),
                min_amount: d("950"),
            },
            &bounds(),
        );
        assert!(accepted.is_ok());
    }

    #[test]
    fn an_unlisted_token_a_stale_feed_or_a_paused_oracle_is_refused() {
        let t = trader();
        let buy = |token: &str| Action::Buy {
            token: token.into(),
            amount: d("10"),
            min_tokens: d("0.05"),
        };
        assert!(t.prepare(&buy("WETH"), &bounds()).is_err());
        let mut stale = bounds();
        stale.round.updated_at = stale.now - 4 * 86_400 - 1;
        assert!(t.prepare(&buy("NVDA"), &stale).is_err());
        let mut weekend = bounds();
        weekend.round.updated_at = weekend.now - 60 * 3_600;
        assert!(
            t.prepare(&buy("NVDA"), &weekend).is_ok(),
            "a weekend-old feed is still used"
        );
        let mut paused = bounds();
        paused.paused = true;
        assert!(t.prepare(&buy("NVDA"), &paused).is_err());
    }

    #[test]
    fn amounts_convert_to_units_rounding_down_and_refuse_negatives() {
        assert_eq!(
            to_units(d("1.2345679"), 6).unwrap(),
            U256::from(1_234_567u64)
        );
        assert!(to_units(d("-1"), 6).is_err());
    }
}
