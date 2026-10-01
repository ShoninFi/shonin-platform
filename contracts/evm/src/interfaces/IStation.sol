// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// One bridge send from a station, kept so money on its way between chains is
/// counted until it lands.
struct Send {
    uint256 toChain;
    uint256 amount;
    uint64 sentAt;
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
    event Pulled(uint256 amount);
    event Pushed(uint256 amount);
    event Deposited(bytes32 indexed driver, uint256 amount);
    event Withdrawn(bytes32 indexed driver, uint256 amount);
    event Bridged(bytes32 indexed bridge, uint256 indexed sequence, uint256 indexed toChain, uint256 amount);
    event Unwound(uint256 fraction, uint256 sentHome, bool blocked);
    event Stopped(bytes32 reason, uint64 haltedAt);

    /// A close inside the band that couldn't bring its worth home: margin a
    /// position was bought on still holds the proceeds at the venue. The
    /// whole unwind is undone.
    error ShortSweep();

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
    /// and send that fraction's dollars home. `blocked` is true, and nothing
    /// is closed, when a market is outside the band. It reverts with
    /// `ShortSweep` when what a venue sends home falls short of the closed
    /// fraction's worth by more than the band.
    function unwind(uint256 fraction) external returns (uint256 sentHome, bool blocked);
    /// What the station holds, at mark and at exit-side prices.
    function value() external view returns (uint256 mark, uint256 exit);

    function spec() external view returns (StationSpec memory);
    function isHome() external view returns (bool);
    function stopped() external view returns (bool);
    function approvedKey(bytes32 driver) external view returns (address);
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
    /// When the vault halted; zero while it runs.
    function haltedAt() external view returns (uint64);
    function haltGrace() external view returns (uint64);
}
