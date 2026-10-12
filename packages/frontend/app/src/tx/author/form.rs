//! What an author types, checked and turned into exactly what the contracts
//! take: allowances in the contracts' own units, dollars in base units, the
//! idle floor in basis points. Nothing passes through a float, and every
//! refusal is said in words under its field.

use alloy_primitives::U256;
use chain::bindings::prices::Allowances as SolAllowances;

use crate::money::{AmountError, format_exact, parse_units};

/// The most an agent may use, in the contracts' own units: what
/// `setAllowances` and `activate` take.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Allowances {
    /// Wasmtime fuel a second.
    pub fuel_per_sec: u64,
    pub memory_mib: u32,
    pub requests_per_min: u32,
    pub bytes_per_day: u64,
    pub streams: u32,
    pub inference_ms_per_min: u32,
    /// Gas, in the dollar's base units a day.
    pub gas_per_day: u64,
}

impl Allowances {
    /// As the contracts' bindings take them.
    pub fn to_sol(self) -> SolAllowances {
        SolAllowances {
            fuelPerSec: self.fuel_per_sec,
            memoryMib: self.memory_mib,
            requestsPerMin: self.requests_per_min,
            bytesPerDay: self.bytes_per_day,
            streams: self.streams,
            inferenceMsPerMin: self.inference_ms_per_min,
            gasPerDay: self.gas_per_day,
        }
    }

    /// As a contract returned them.
    pub fn from_sol(sol: &SolAllowances) -> Self {
        Self {
            fuel_per_sec: sol.fuelPerSec,
            memory_mib: sol.memoryMib,
            requests_per_min: sol.requestsPerMin,
            bytes_per_day: sol.bytesPerDay,
            streams: sol.streams,
            inference_ms_per_min: sol.inferenceMsPerMin,
            gas_per_day: sol.gasPerDay,
        }
    }
}

/// The allowances as the form's fields hold them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AllowanceText {
    /// Millions of Wasmtime fuel a second.
    pub fuel_millions: String,
    /// MiB.
    pub memory_mib: String,
    /// A minute.
    pub requests_per_min: String,
    /// MB (a million bytes) a day.
    pub data_mb: String,
    /// At once.
    pub streams: String,
    /// Milliseconds a minute.
    pub inference_ms: String,
    /// Dollars a day.
    pub gas_dollars: String,
}

/// Which allowance field a refusal is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AllowanceField {
    Fuel,
    Memory,
    Requests,
    Data,
    Streams,
    Inference,
    Gas,
}

/// A whole number of some unit that fits the contract's field.
fn whole<T: TryFrom<u128>>(text: &str) -> Result<T, String> {
    let value = parse_units(text, 0).map_err(|error| match error {
        AmountError::TooManyDecimals { .. } => "Use a whole number.".to_string(),
        other => other.words().replace("an amount", "a number"),
    })?;
    u128::try_from(value)
        .ok()
        .and_then(|value| T::try_from(value).ok())
        .ok_or_else(|| "That number is too large.".to_string())
}

/// A decimal of `decimals` places as a whole number of its smallest unit,
/// fitting a `u64`.
fn scaled(text: &str, decimals: u8) -> Result<u64, String> {
    let value = parse_units(text, decimals)
        .map_err(|error| error.words().replace("an amount", "a number"))?;
    u64::try_from(value).map_err(|_| "That number is too large.".to_string())
}

impl AllowanceText {
    /// The fields as a set of allowances shows them.
    pub fn of(allowances: &Allowances, asset_decimals: u8) -> Self {
        Self {
            fuel_millions: format_exact(U256::from(allowances.fuel_per_sec), 6, 0).replace(',', ""),
            memory_mib: allowances.memory_mib.to_string(),
            requests_per_min: allowances.requests_per_min.to_string(),
            data_mb: format_exact(U256::from(allowances.bytes_per_day), 6, 0).replace(',', ""),
            streams: allowances.streams.to_string(),
            inference_ms: allowances.inference_ms_per_min.to_string(),
            gas_dollars: format_exact(U256::from(allowances.gas_per_day), asset_decimals, 2)
                .replace(',', ""),
        }
    }

    /// The allowances these fields say, or each field's refusal.
    pub fn parse(&self, asset_decimals: u8) -> Result<Allowances, Vec<(AllowanceField, String)>> {
        let mut errors = Vec::new();
        let mut take = |field: AllowanceField, result: Result<u64, String>| match result {
            Ok(value) => value,
            Err(why) => {
                errors.push((field, why));
                0
            }
        };
        let fuel = take(AllowanceField::Fuel, scaled(&self.fuel_millions, 6));
        let memory = take(
            AllowanceField::Memory,
            whole::<u32>(&self.memory_mib).map(u64::from),
        );
        let requests = take(
            AllowanceField::Requests,
            whole::<u32>(&self.requests_per_min).map(u64::from),
        );
        let data = take(AllowanceField::Data, scaled(&self.data_mb, 6));
        let streams = take(
            AllowanceField::Streams,
            whole::<u32>(&self.streams).map(u64::from),
        );
        let inference = take(
            AllowanceField::Inference,
            whole::<u32>(&self.inference_ms).map(u64::from),
        );
        let gas = take(
            AllowanceField::Gas,
            scaled(&self.gas_dollars, asset_decimals),
        );
        if !errors.is_empty() {
            return Err(errors);
        }
        // Each fits its field: the u32 ones were parsed as u32.
        Ok(Allowances {
            fuel_per_sec: fuel,
            memory_mib: memory as u32,
            requests_per_min: requests as u32,
            bytes_per_day: data,
            streams: streams as u32,
            inference_ms_per_min: inference as u32,
            gas_per_day: gas,
        })
    }
}

/// The starting shapes an author picks between, in the contracts' units.
pub fn shape(name: &str) -> Allowances {
    let shape = shared::Allowances::shape(name);
    Allowances {
        fuel_per_sec: shape.fuel_per_sec,
        memory_mib: shape.memory_mib,
        requests_per_min: shape.requests_per_min,
        bytes_per_day: shape.bytes_per_day,
        streams: shape.streams,
        inference_ms_per_min: shape.inference_ms_per_min,
        // Whole cents in the shapes; read exactly from their text.
        gas_per_day: scaled(&format!("{:.2}", shape.gas_per_day), 6).unwrap_or(0),
    }
}

/// A dollar amount typed into a field, checked: the amount, or what to say.
pub fn dollars(text: &str, decimals: u8) -> Result<U256, String> {
    parse_units(text, decimals).map_err(|error| error.words())
}

/// The starting tank: at least the factory's minimum plus whatever stations
/// on other chains cost (none, for a vault on one chain), and no more than
/// the wallet holds when that's known.
pub fn starting_tank(
    text: &str,
    decimals: u8,
    minimum: U256,
    station_costs: U256,
    balance: Option<U256>,
) -> Result<U256, String> {
    let tank = dollars(text, decimals)?;
    let least = minimum.saturating_add(station_costs);
    if tank < least {
        return Err(format!(
            "The starting tank must be at least {}.",
            crate::money::format::dollars_exact(least, decimals)
        ));
    }
    if let Some(have) = balance
        && tank > have
    {
        return Err(format!(
            "That's more than your wallet holds ({}).",
            crate::money::format::dollars(have, decimals, crate::money::Rounding::Down)
        ));
    }
    Ok(tank)
}

/// An amount to send that must be more than zero and no more than `most`
/// (the wallet's balance, or the tank's).
pub fn positive_up_to(
    text: &str,
    decimals: u8,
    most: Option<U256>,
    over: &str,
) -> Result<U256, String> {
    let amount = dollars(text, decimals)?;
    if amount.is_zero() {
        return Err("Type an amount more than zero.".to_string());
    }
    if let Some(most) = most
        && amount > most
    {
        return Err(over.to_string());
    }
    Ok(amount)
}

/// Shares typed as a decimal, in the vault's 18-decimal units.
pub fn shares(text: &str, held: U256) -> Result<U256, String> {
    let shares = parse_units(text, 18).map_err(|error| error.words())?;
    if shares.is_zero() {
        return Err("Type a number of shares more than zero.".to_string());
    }
    if shares > held {
        return Err("That's more shares than you hold.".to_string());
    }
    Ok(shares)
}

/// The idle floor typed as a percentage (`5`, `0.5`, `12.25`), in basis
/// points: at most two decimals, at most 100%.
pub fn floor_bps(text: &str) -> Result<u16, String> {
    let text = text.trim().trim_end_matches('%').trim_end();
    let bps = parse_units(text, 2).map_err(|error| match error {
        AmountError::TooManyDecimals { .. } => {
            "Use at most two digits after the point.".to_string()
        }
        AmountError::Empty => "Type a percentage.".to_string(),
        other => other.words().replace("an amount", "a percentage"),
    })?;
    match u16::try_from(bps) {
        Ok(bps) if bps <= 10_000 => Ok(bps),
        _ => Err("The idle floor can't be more than 100%.".to_string()),
    }
}

/// Basis points as a percentage, as few digits as it needs: `5%`, `0.5%`,
/// `12.25%`.
pub fn percent(bps: u16) -> String {
    let text = format_exact(U256::from(bps), 2, 0).replace(',', "");
    format!("{text}%")
}

/// Whether the typed name confirms retiring `name`: exactly the name, with
/// spaces at either end and runs of spaces inside forgiven.
pub fn name_matches(typed: &str, name: &str) -> bool {
    let tidy = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = tidy(name);
    !name.is_empty() && tidy(typed) == name
}

/// Whole days a tank of `balance` lasts at most at `daily` a day: rounded
/// down, so the runway is never overstated. None when nothing is charged.
pub fn runway_days(balance: U256, daily: U256) -> Option<U256> {
    (!daily.is_zero()).then(|| balance / daily)
}

/// Whether the tank covers fewer than `days` days at `daily` a day: the
/// retire dialog's caution, compared exactly.
pub fn covers_less_than(balance: U256, daily: U256, days: u64) -> bool {
    !daily.is_zero() && balance < daily.saturating_mul(U256::from(days))
}

/// The tank amounts the activation page offers: the minimum, and a week and
/// two weeks at the upper bound rounded up to the cent, never under the
/// minimum.
pub fn quick_amounts(minimum: U256, daily: U256, decimals: u8) -> [(&'static str, U256); 3] {
    let cent = crate::money::units::pow10(decimals.saturating_sub(2));
    let up_to_cent = |amount: U256| amount.div_ceil(cent) * cent;
    let at_least = |amount: U256| up_to_cent(amount).max(minimum);
    [
        ("minimum", minimum),
        ("1 week", at_least(daily.saturating_mul(U256::from(7u8)))),
        ("2 weeks", at_least(daily.saturating_mul(U256::from(14u8)))),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(units: u64) -> U256 {
        U256::from(units)
    }

    #[test]
    fn allowances_read_back_exactly_as_the_contract_takes_them() {
        let modest = shape("modest");
        assert_eq!(modest.fuel_per_sec, 10_000_000);
        assert_eq!(modest.gas_per_day, 1_000_000);
        let text = AllowanceText::of(&modest, 6);
        assert_eq!(text.fuel_millions, "10");
        assert_eq!(text.data_mb, "50");
        assert_eq!(text.gas_dollars, "1.00");
        assert_eq!(text.parse(6), Ok(modest));
        let quoting = shape("quoting");
        assert_eq!(quoting.gas_per_day, 5_000_000);
        assert_eq!(AllowanceText::of(&quoting, 6).parse(6), Ok(quoting));
    }

    #[test]
    fn fractional_allowances_convert_without_rounding() {
        let mut text = AllowanceText::of(&shape("modest"), 6);
        text.fuel_millions = "2.5".to_string();
        text.data_mb = "0.000001".to_string();
        text.gas_dollars = "0.015".to_string();
        let parsed = text.parse(6).unwrap();
        assert_eq!(parsed.fuel_per_sec, 2_500_000);
        assert_eq!(parsed.bytes_per_day, 1);
        assert_eq!(parsed.gas_per_day, 15_000);
    }

    #[test]
    fn a_bad_allowance_field_is_named_and_nothing_is_guessed() {
        let mut text = AllowanceText::of(&shape("modest"), 6);
        text.memory_mib = "64.5".to_string();
        text.streams = "1,000".to_string();
        text.gas_dollars = "0.0000001".to_string();
        text.requests_per_min = "4294967296".to_string();
        let errors = text.parse(6).unwrap_err();
        let fields: Vec<AllowanceField> = errors.iter().map(|(field, _)| *field).collect();
        assert_eq!(
            fields,
            vec![
                AllowanceField::Memory,
                AllowanceField::Requests,
                AllowanceField::Streams,
                AllowanceField::Gas
            ]
        );
        assert_eq!(errors[0].1, "Use a whole number.");
        assert_eq!(errors[1].1, "That number is too large.");
    }

    #[test]
    fn the_starting_tank_is_at_least_the_minimum_plus_station_costs() {
        let minimum = usd(10_000_000);
        assert_eq!(
            starting_tank("10", 6, minimum, U256::ZERO, None),
            Ok(usd(10_000_000))
        );
        assert_eq!(
            starting_tank("9.999999", 6, minimum, U256::ZERO, None),
            Err("The starting tank must be at least $10.00.".to_string())
        );
        assert_eq!(
            starting_tank("10", 6, minimum, usd(2_500_000), None),
            Err("The starting tank must be at least $12.50.".to_string())
        );
        assert_eq!(
            starting_tank("12.5", 6, minimum, usd(2_500_000), None),
            Ok(usd(12_500_000))
        );
        assert_eq!(
            starting_tank("210", 6, minimum, U256::ZERO, Some(usd(200_000_000))),
            Err("That's more than your wallet holds ($200.00).".to_string())
        );
        assert!(starting_tank("1,000", 6, minimum, U256::ZERO, None).is_err());
        assert!(starting_tank("10.0000001", 6, minimum, U256::ZERO, None).is_err());
    }

    #[test]
    fn the_idle_floor_is_a_percentage_with_two_decimals_at_most_and_never_over_100() {
        assert_eq!(floor_bps("5"), Ok(500));
        assert_eq!(floor_bps("5%"), Ok(500));
        assert_eq!(floor_bps("0"), Ok(0));
        assert_eq!(floor_bps("0.5"), Ok(50));
        assert_eq!(floor_bps("12.25"), Ok(1_225));
        assert_eq!(floor_bps("100"), Ok(10_000));
        assert!(floor_bps("100.01").is_err());
        assert!(floor_bps("1000000").is_err());
        assert_eq!(
            floor_bps("1.125"),
            Err("Use at most two digits after the point.".to_string())
        );
        assert!(floor_bps("").is_err());
        assert!(floor_bps("-1").is_err());
        assert_eq!(percent(500), "5%");
        assert_eq!(percent(50), "0.5%");
        assert_eq!(percent(1_225), "12.25%");
        assert_eq!(percent(0), "0%");
    }

    #[test]
    fn an_amount_to_send_is_more_than_zero_and_within_what_there_is() {
        assert_eq!(
            positive_up_to("25.5", 6, Some(usd(100_000_000)), "over"),
            Ok(usd(25_500_000))
        );
        assert_eq!(
            positive_up_to("0", 6, None, "over"),
            Err("Type an amount more than zero.".to_string())
        );
        assert_eq!(
            positive_up_to("100.000001", 6, Some(usd(100_000_000)), "over"),
            Err("over".to_string())
        );
    }

    #[test]
    fn shares_are_read_in_the_vaults_eighteen_decimals_and_never_more_than_held() {
        let held = U256::from(1_642_800_000_000_000_000_000u128);
        assert_eq!(
            shares("100.5", held),
            Ok(U256::from(100_500_000_000_000_000_000u128))
        );
        assert!(shares("1642.9", held).is_err());
        assert!(shares("0", held).is_err());
    }

    #[test]
    fn the_retire_name_must_match_exactly_but_forgives_spacing() {
        assert!(name_matches("Basis Harvester", "Basis Harvester"));
        assert!(name_matches("  Basis   Harvester ", "Basis Harvester"));
        assert!(!name_matches("basis harvester", "Basis Harvester"));
        assert!(!name_matches("Basis", "Basis Harvester"));
        assert!(!name_matches("", ""));
    }

    #[test]
    fn the_runway_rounds_down_and_the_caution_compares_exactly() {
        // $1,212.40 at $29.50 a day: 41 whole days.
        assert_eq!(
            runway_days(usd(1_212_400_000), usd(29_500_000)),
            Some(U256::from(41u8))
        );
        assert_eq!(runway_days(usd(1), U256::ZERO), None);
        // Seven days at $1.0585 is $7.4095.
        assert!(covers_less_than(usd(7_409_499), usd(1_058_500), 7));
        assert!(!covers_less_than(usd(7_409_500), usd(1_058_500), 7));
        assert!(!covers_less_than(usd(1), U256::ZERO, 7));
    }

    #[test]
    fn quick_amounts_are_whole_cents_rounded_up_and_never_under_the_minimum() {
        let amounts = quick_amounts(usd(10_000_000), usd(1_058_500), 6);
        assert_eq!(amounts[0].1, usd(10_000_000));
        assert_eq!(amounts[1].1, usd(10_000_000));
        // 14 × $1.0585 = $14.819, offered as $14.82.
        assert_eq!(amounts[2].1, usd(14_820_000));
        let amounts = quick_amounts(usd(10_000_000), usd(30_000_000), 6);
        assert_eq!(amounts[1].1, usd(210_000_000));
        assert_eq!(amounts[2].1, usd(420_000_000));
    }
}
