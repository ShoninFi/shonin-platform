// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// One call a station makes on a driver's or a bridge's behalf.
struct Call {
    address target;
    uint256 value;
    bytes data;
}

/// Who stops a vault's keys sending money anywhere but its own accounts.
enum Custody {
    /// The venue: the key it is given can trade, and only the station can
    /// move money out.
    Venue,
    /// The station's own account, whose rules allow only the venue's calls.
    Station
}

/// Who closes positions when someone leaves.
enum ExitPath {
    /// The station, inside the exit band, in the leaver's own transaction.
    Contract,
    /// An unwinder, because the venue's state is not readable on-chain.
    Unwinder,
    /// The station, but the venue acts on its calls after the transaction
    /// (a venue that executes queued actions after the block): a close is
    /// placed in one transaction, proved and swept in a later one, and its
    /// dollars delivered home once they land. The driver also implements
    /// `IDelayedDriver`.
    Delayed
}

/// How a station's holdings at the venue are valued.
enum ValuationKind {
    OnChain,
    AttestedRead
}

/// The on-chain half of a driver: it prepares the calls a station makes to one
/// venue, and values what the station holds there.
///
/// A driver holds nothing and keeps no per-vault state, so one deployment
/// serves every station on its chain. It never calls anything itself: every
/// function is a view that returns calls, and the station checks each call's
/// target against `targets()` before making it. A station pins the driver
/// approved when it was created and uses it for good, so a newer version
/// reaches only stations created after it is approved.
interface IDriver {
    /// The name `shonin.toml` writes, zero-padded.
    function name() external view returns (bytes32);
    /// Every contract this driver's calls may reach. A call to the dollar is
    /// only ever an approval of one of these.
    function targets() external view returns (address[] memory);
    /// Who the venue's terms exclude, as short codes: country codes, or
    /// `SANCTIONED`. Sorted, without repeats.
    function restricted() external view returns (bytes32[] memory);
    function custody() external view returns (Custody);
    function exitPath() external view returns (ExitPath);
    function valuation() external view returns (ValuationKind);

    /// Let `key` trade the station's account until `validUntil`.
    function approveKey(
        address station,
        address key,
        uint64 validUntil
    ) external view returns (Call[] memory);
    function revokeKey(address station, address key) external view returns (Call[] memory);
    /// Move `amount` of the station's idle dollars into its venue account.
    function deposit(address station, uint256 amount) external view returns (Call[] memory);
    /// And back.
    function withdraw(address station, uint256 amount) external view returns (Call[] memory);
    /// Close `fraction` (1e18 is all) of every position, each order no worse
    /// than `bandBps` from the venue's own price; at 10,000 or more, at any
    /// price, never blocked. `blocked` is true, with no
    /// calls, when a market is outside the band: nothing is closed then, so a
    /// leaver is never sold through a market that has run away.
    function close(
        address station,
        uint256 fraction,
        uint16 bandBps
    ) external view returns (Call[] memory calls, bool blocked);
    /// Bring `amount` of the account's dollars back to the station as idle
    /// dollars, after a close has freed them, or as much as there is.
    function sweep(address station, uint256 amount) external view returns (Call[] memory);
    /// What the station holds at the venue, at mark and at exit-side prices.
    function valueOf(address station) external view returns (uint256 mark, uint256 exit);
}

/// The on-chain half of a bridge: it prepares the calls that send a station's
/// dollars to another chain. The station passes its own address as the
/// recipient, which is the same address on every EVM chain.
interface IBridge {
    function name() external view returns (bytes32);
    function targets() external view returns (address[] memory);
    function send(
        address station,
        uint256 toChain,
        address recipient,
        uint256 amount,
        bytes calldata extra
    ) external view returns (Call[] memory);
}

/// What a station needs from a driver whose venue acts after the
/// transaction: a close can't be checked by what it brought home in the same
/// transaction, so it is checked against the positions instead.
interface IDelayedDriver {
    /// What the station holds in each market the driver knows, in the
    /// venue's own units, in a fixed order: what `closed` compares against.
    function exposure(address station) external view returns (int256[] memory);
    /// Whether a close of `fraction` placed when the station held `before`
    /// has happened: every position it would have ordered is at least that
    /// much smaller now, on the same side.
    function closed(
        address station,
        int256[] calldata before,
        uint256 fraction
    ) external view returns (bool);
    /// What the station's money parked in the venue's own vaults (`held`) is
    /// worth.
    function heldValue(address station, address[] calldata held) external view returns (uint256);
    /// The calls that bring the station's money in one of the venue's own
    /// vaults back to its account, as soon as the vault's lock allows (none
    /// before), and what it holds there now.
    function reclaim(
        address station,
        address held
    ) external view returns (Call[] memory calls, uint256 equity);
}
