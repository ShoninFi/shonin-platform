// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// One bridge send from a station, kept so money on its way between chains is
/// counted until it lands.
struct Send {
    uint256 toChain;
    uint256 amount;
    uint64 sentAt;
}

/// A close through a delayed driver, in progress: placed, then proved,
/// measured and swept, then delivered home, in whole or in part, once its
/// dollars have landed or its delivery window has passed. Every stage ends
/// within its windows.
struct Stage {
    /// The fraction of every position being closed (1e18 is all).
    uint256 fraction;
    /// What the stage holds at the station for the leaver: the fraction of
    /// the idle dollars and what other venues brought home at the stage.
    /// The part of it delivered is the part of the close delivered.
    uint256 owed;
    /// The most the delayed venues' part can be measured at: the fraction's
    /// worth there at exit-side prices when it was placed.
    uint256 estimate;
    /// What the delayed venues' part was measured to have fetched, once
    /// proved, never more than `part` of the estimate.
    uint256 fetched;
    /// What the measure fell below nothing by: a loss the stage's own sale
    /// made, owed by the leaver.
    uint256 cost;
    /// The part of the close that happened (1e18 is all), once proved.
    uint256 part;
    /// The station's `arrivals` when it was placed.
    uint256 arrivedAt;
    uint64 stagedAt;
    uint64 stagedBlock;
    /// When it was proved and its dollars asked for.
    uint64 sweptAt;
    uint64 sweptBlock;
    /// How long it may wait to be proved, and then for its dollars: the
    /// longest delivery window of its drivers.
    uint64 window;
    /// How long after it was placed it may first be proved: the longest
    /// settle delay of its drivers.
    uint64 delay;
    /// The moment it may no longer be proved: from then on the agent may
    /// trade the venue accounts again, so nothing read there tells the
    /// close from the agent's trading, and the stage can only lapse. Until
    /// it is proved or this has passed, the attested executor neither
    /// places nor changes an order nor moves dollars at its venues.
    uint64 provedBy;
    /// Placed by the vault for one of its claims or one holder's units, and
    /// delivered through the vault's own door for them.
    bool forVault;
}

/// What a station is told about itself when it is created.
struct StationSpec {
    /// The chain its vault lives on, by chain id.
    uint256 vaultChain;
    address vault;
    /// The bundle the vault runs: only a key blessed for it may trade here.
    bytes32 bundleHash;
    /// The drivers the strategy declared for this chain.
    bytes32[] drivers;
    /// The bridges it declared.
    bytes32[] bridges;
    /// The other chains the vault trades on: with the vault's own, the only
    /// ones a bridge send may go to.
    uint256[] chains;
    /// How far from the venue's own price a closing order may fill.
    uint16 exitBandBps;
}

/// A vault's account-holder on one chain.
///
/// It owns the vault's venue accounts on that chain and makes every venue call
/// itself, through the drivers the registry approves, checking each call's
/// target. The vault never trades: its home station does, and so do its
/// stations on other chains, each at the same address. Money only ever moves
/// between the vault, its stations and their venue accounts.
interface IStation {
    event KeyApproved(bytes32 indexed driver, address indexed key, uint64 validUntil);
    event KeyRevoked(bytes32 indexed driver, address indexed key);
    /// A best-effort revoke (at a stop, or a lapsed key's replacement) didn't
    /// go through at the venue; anyone may withdraw the key with `revokeKey`.
    event RevokeSkipped(bytes32 indexed driver, address indexed key);
    event Pulled(uint256 amount);
    event Pushed(uint256 amount);
    event Deposited(bytes32 indexed driver, uint256 amount);
    event Withdrawn(bytes32 indexed driver, uint256 amount);
    event Bridged(bytes32 indexed bridge, uint256 indexed sequence, uint256 indexed toChain, uint256 amount);
    event Unwound(uint256 fraction, uint256 sentHome, bool blocked);
    event Stopped(bytes32 reason, uint64 haltedAt);
    /// A driver failed during a stopped station's full unwind; the others
    /// went ahead.
    event DriverFailed(bytes32 indexed driver);
    /// A close through a delayed driver was placed; it is proved, measured,
    /// swept and delivered by `settleClose` in later blocks.
    event CloseStaged(uint256 fraction, uint256 owed, uint256 estimate);
    /// Proved and measured at `fetched`, and `asked` of it asked for from
    /// the venue; or, a delivery window on, asked for again.
    event CloseSwept(uint256 fraction, uint256 fetched, uint256 asked);
    /// A staged close nothing could be measured for by the end of its
    /// window (no reading of the venue after it, or a position the venue
    /// changed by more than her orders could have), one measured below what
    /// her orders could have fetched at their limits, one not proved by its
    /// deadline, or one a stopped station's full unwind took the place of:
    /// nothing is paid from it, and whatever it sold stays at the station,
    /// everyone's.
    event CloseLapsed(uint256 fraction);
    event CloseDelivered(uint256 fraction, uint256 amount);
    /// How a staged close ended: `part` of it delivered (1e18 is all), and
    /// `cost` dollars of loss its own sale made, owed by whom it was for.
    event CloseSettled(uint256 part, uint256 cost);
    /// Money in one of the venue's own vaults is counted, and brought back
    /// when its lock allows; trading through the driver has ended.
    event Reclaimed(bytes32 indexed driver, address indexed venueVault, uint256 equity);

    /// A close inside the band that couldn't bring its worth home: margin a
    /// position was bought on still holds the proceeds at the venue. The
    /// whole unwind is undone.
    error ShortSweep();
    /// A station's drivers all close in the transaction, or all after it.
    error MixedExits();

    /// Let `key` trade through `driver`, replacing the key approved for it
    /// now. Anyone may call it; it obeys only the registry.
    function approveKey(bytes32 driver, address key) external;
    /// Withdraw a key whose blessing has lapsed or was revoked, or any key
    /// once the station has stopped. Anyone may call it.
    function revokeKey(bytes32 driver, address key) external;
    /// Home station: take `amount` of the vault's cash, inside its idle floor.
    function pull(uint256 amount) external;
    /// Home station: send `amount` of idle dollars to the vault.
    function push(uint256 amount) external;
    function deposit(bytes32 driver, uint256 amount) external;
    function withdraw(bytes32 driver, uint256 amount) external;
    /// Send `amount` over a declared bridge to the same address on `toChain`.
    function bridge(bytes32 name, uint256 toChain, uint256 amount, bytes calldata extra) external;
    /// Home station: stop for good, once the vault has halted and its grace
    /// has passed. Anyone may call it.
    function stop() external;
    /// Another chain's station: stop, on the word of a key blessed for the
    /// vault, carried from the vault's chain.
    function stop(bytes32 reason, uint64 haltedAt, bytes calldata signature) external;
    /// Close `fraction` (1e18 is all) of every position inside the exit band
    /// and send that fraction's dollars home. A stopped station's full
    /// unwind closes each venue on its own inside `stoppedBand()`, skipping
    /// one that fails, and sends everything idle home. `blocked` is true, and nothing
    /// is closed, when a market is outside the band. It reverts with
    /// `ShortSweep` when what a venue sends home falls short of the closed
    /// fraction's worth by more than the band. On a station with a delayed
    /// driver a partial close is staged instead: nothing goes home now, and
    /// `blocked` is true until `settleClose` has delivered it. On the home
    /// station only the vault stages one, through `stageClose`.
    function unwind(uint256 fraction) external returns (uint256 sentHome, bool blocked);
    /// Home station, the vault only: close `fraction` for one of its claims
    /// or one holder's units. Where a delayed driver holds anything worth
    /// closing, the close is staged (`staged`), one at a time, and delivered
    /// to the vault through `delivered` once its dollars have landed, or
    /// handed back through `unstaged`; otherwise it is `unwind`'s close.
    function stageClose(uint256 fraction) external returns (uint256 sentHome, bool blocked, bool staged);
    /// What the station holds, at mark and at exit-side prices.
    function value() external view returns (uint256 mark, uint256 exit);
    /// Move a close staged through a delayed driver on; anyone may call it.
    /// Once its settle delay has passed: prove and measure it, wholly
    /// closed or, its window over, for the part that closed, and ask the
    /// venues for what it fetched. Deliver it once those dollars have
    /// landed, or, a delivery window after the ask, deliver the part that
    /// has. A stage nothing could be measured for by the end of its window
    /// lapses.
    function settleClose() external;
    function stage() external view returns (Stage memory);
    /// Every dollar that has reached the station other than through its own
    /// calls (a venue's send or a pending withdrawal paid out, a bridge, a
    /// gift), counted up for good.
    function arrivals() external view returns (uint256);
    /// What the station has put into a venue through `deposit`, counted up
    /// for good, and when it last did.
    function venueFlows(bytes32 driver) external view returns (uint256 sent, uint64 depositedAt);
    /// When dollars last moved between the station and a venue through a
    /// driver: the executor's deposit or withdrawal, or a stopped unwind
    /// through an account worth more than a dollar.
    function movedAt(bytes32 driver) external view returns (uint64);
    /// When a stopped station's unwind first ran through a driver at the
    /// full band (any price); zero until then.
    function fullUnwindAt(bytes32 driver) external view returns (uint64);
    /// Count, bring back, and end trading over, money the station's account
    /// put into one of a venue's own vaults. Anyone may call it.
    function reclaim(bytes32 driver, address venueVault) external;
    function heldIn(bytes32 driver) external view returns (address[] memory);
    function reclaimed(bytes32 driver) external view returns (bool);
    /// Whether any market a driver holds is further from the venue's own
    /// price than the exit band, so a close of it would be refused, or a
    /// delayed driver's value can't be trusted now: either voids a
    /// valuation.
    function outsideBand() external view returns (bool);

    function spec() external view returns (StationSpec memory);
    function isHome() external view returns (bool);
    function stopped() external view returns (bool);
    function approvedKey(bytes32 driver) external view returns (address);
    /// The implementation pinned at creation for a declared driver or bridge.
    function pinnedDriver(bytes32 driver) external view returns (address);
    function pinnedBridge(bytes32 name) external view returns (address);
    /// How long a stopped station's unwind keeps the exit band before it
    /// starts doubling, and between doublings.
    function WIDEN_AFTER() external view returns (uint64);
    /// When it stopped; zero while it runs.
    function stoppedAt() external view returns (uint64);
    /// The band its full unwind closes inside now: the exit band, widening
    /// after it has been stopped a while.
    function stoppedBand() external view returns (uint16);
    /// A stopped station's full unwind through the named drivers only, then
    /// everything idle home; anyone may call it.
    function unwindStopped(bytes32[] calldata drivers) external returns (uint256 sentHome, bool blocked);
    /// Only the station itself calls it, for a best-effort revoke.
    function revokeOnce(bytes32 driver, address key) external;
    /// Only the station itself calls it, during a stopped full unwind.
    function closeAll(bytes32 driver, uint16 band) external returns (bool outside);
    function sends() external view returns (Send[] memory);
    function sendCount() external view returns (uint256);
    function sendAt(uint256 index) external view returns (Send memory);
    function stopDigest(bytes32 reason, uint64 haltedAt) external view returns (bytes32);
}

/// Creates a vault's stations, at one address on every chain.
interface IStationFactory {
    event StationCreated(uint256 indexed vaultChain, address indexed vault, address station);

    /// Set what differs between chains, once, by the key the creation code
    /// names.
    function initialize(
        address attestation_,
        address asset_,
        address vaultFactory_,
        address implementation_
    ) external;

    /// The vault's own chain, called by the vault factory inside `activate`.
    function createHome(
        address vault,
        bytes32 bundleHash,
        bytes32[] calldata drivers,
        bytes32[] calldata bridges,
        uint256[] calldata chains,
        uint16 exitBandBps
    ) external returns (address);
    /// Another chain, called by a key blessed there as the vault's executor,
    /// with the builder's signature over this chain's station.
    function create(StationSpec calldata spec, bytes calldata builderSignature) external returns (address);
    /// Where the vault's station is, or will be, on every chain.
    function stationOf(uint256 vaultChain, address vault) external view returns (address);
    function exists(uint256 vaultChain, address vault) external view returns (bool);
    /// What a builder signs for one chain's station of one bundle.
    function stationDigest(
        uint256 vaultChain,
        bytes32 bundleHash,
        bytes32[] calldata drivers,
        bytes32[] calldata bridges,
        uint256[] calldata chains,
        uint16 exitBandBps
    ) external view returns (bytes32);

    function attestation() external view returns (address);
    function asset() external view returns (address);
    function vaultFactory() external view returns (address);
    function implementation() external view returns (address);
}

/// What a home station needs from its vault.
interface IStationVault {
    /// Hand `amount` of cash to the home station, if the idle floor allows.
    function toStation(uint256 amount) external;
    /// Take `amount` from the home station into cash.
    function fromStation(uint256 amount) external;
    /// Take `amount` from the home station: the part of the close it staged
    /// for the vault that was delivered, `part` of it (1e18 is all), paid to
    /// the claim or the holder the vault staged it for and no one else.
    /// `cost` is a loss the close's own sale made, owed by them; `stake` is
    /// what the station's side of their stake was worth at exit-side prices
    /// at the stage, the rate any of it left unpaid is taken at.
    function delivered(uint256 amount, uint256 part, uint256 cost, uint256 stake) external;
    /// The close the home station staged for the vault ended without a
    /// delivery: nothing could be measured in time, or a stopped station's
    /// full unwind took it over.
    function unstaged() external;
    /// When the vault halted; zero while it runs.
    function haltedAt() external view returns (uint64);
    function haltGrace() external view returns (uint64);
    /// Whether a valuation is waiting for other chains' posts.
    function valuing() external view returns (bool);
    /// Whether a priced valuation still has requests to settle.
    function unsettled() external view returns (bool);
}
