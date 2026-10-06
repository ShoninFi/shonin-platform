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
    /// placed in one transaction, proved, measured and swept in a later
    /// one, and its dollars delivered home once they land. The driver also
    /// implements `IDelayedDriver`.
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
/// transaction, so it is checked against the positions instead, and what it
/// fetched is measured against what the account held when it was placed.
interface IDelayedDriver {
    /// What the station holds in each market the driver knows, in the
    /// venue's own units, in a fixed order: what `closed` compares against
    /// and `measure` reprices. A driver may add to it what it needs to read
    /// it again (when it was read, the prices it was read at).
    function exposure(address station) external view returns (int256[] memory);
    /// The station's dollars through the driver, signed (a perp ledger can
    /// be negative): with `exposure`, everything the account is worth.
    function ledger(address station) external view returns (int256);
    /// What part of a close of `fraction`, placed when the station held
    /// `before`, has happened (1e18 is all of it): for each market it would
    /// have ordered, how much of what it ordered the position is smaller
    /// now, on the same side; the smallest of those, and 1e18 where nothing
    /// needed ordering. `known` is false while the driver can't yet tell
    /// from what the venue did after `since` (the stage plus its settle
    /// delay): a reading made before then says nothing about the close.
    function closedPart(
        address station,
        int256[] calldata before,
        uint256 fraction,
        uint64 since
    ) external view returns (uint256 part, bool known);
    /// What a close of `fraction` placed when the account held `dollars`
    /// (its `ledger`) and `before` (its `exposure`) fetched for `part` of
    /// it (1e18 is all), by the venue's prices now: what the account is
    /// worth now, less what the rest of the account as it was
    /// (1 - `fraction` x `part`) would be worth now, rounded against the
    /// leaver. A market move since cancels out, and so does a trade made
    /// since at the price now; every dollar of slippage of what the close
    /// sold falls on it. Never below what the close's own orders could have
    /// lost at their limits (`bandBps` of what they ordered, at the prices
    /// now), so a loss nobody's close could have made isn't the leaver's.
    /// It may be negative; the station adds what reached it from the venue
    /// meanwhile.
    function measure(
        address station,
        int256 dollars,
        int256[] calldata before,
        uint256 fraction,
        uint256 part,
        uint16 bandBps
    ) external view returns (int256);
    /// How long a close through the driver may take to be proved, and its
    /// dollars to reach the station once asked for.
    function deliveryWindow() external view returns (uint64);
    /// How long the venue may take to act on a call queued in a
    /// transaction: a close is proved no sooner than this after it was
    /// placed.
    function settleDelay() external view returns (uint64);
    /// Whether what `valueOf` says can be trusted now: false while dollars
    /// moved between the station and the venue may be counted in neither,
    /// or while the venue's state is known only from a stale reading.
    function priced(address station) external view returns (bool);
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
