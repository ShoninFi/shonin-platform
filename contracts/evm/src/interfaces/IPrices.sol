// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// The most an agent may use, set by its author on the live vault. The vault
/// only believes a usage bill up to these allowances at the published prices.
struct Allowances {
    /// Wasmtime fuel a second; a million a second stands for one core.
    uint64 fuelPerSec;
    uint32 memoryMib;
    /// Enforced by the executor, not priced.
    uint32 requestsPerMin;
    uint64 bytesPerDay;
    /// Enforced by the executor, not priced.
    uint32 streams;
    uint32 inferenceMsPerMin;
    /// Gas, in the asset's units (6 decimals) a day, charged at face value.
    uint64 gasPerDay;
}

/// What each unit costs, in dollars scaled by 1e18, so that a fraction of a
/// cent per second of inference survives.
struct UnitPrices {
    uint128 vcpuHour;
    uint128 gibHour;
    uint128 gbEgress;
    uint128 inferenceSecond;
}

/// One entry in the price history: these prices, from this time on.
struct PricePeriod {
    uint64 effectiveFrom;
    UnitPrices prices;
}

/// The published price list every vault is charged at. A change is announced
/// with notice before it takes effect, and the same prices apply to every
/// vault, so no author's costs can be changed without warning.
interface IPrices {
    event PricesScheduled(uint64 effectiveFrom, UnitPrices prices);

    /// Admin only. Takes effect after the notice period.
    function schedule(UnitPrices calldata prices) external;

    /// The most the allowances could cost between two times, in the asset's
    /// units (6 decimals), splitting the interval at every price change inside
    /// it. Rounds down, in the tank's favour.
    function costOf(Allowances calldata allowances, uint64 from, uint64 to) external view returns (uint256);
    /// A day at the allowances, at the prices in force now.
    function dailyCost(Allowances calldata allowances) external view returns (uint256);

    function current() external view returns (UnitPrices memory);
    function history() external view returns (PricePeriod[] memory);
    function notice() external view returns (uint64);
    function admin() external view returns (address);
}
