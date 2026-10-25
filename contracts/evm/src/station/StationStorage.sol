// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry} from "../interfaces/IAttestationRegistry.sol";
import {Call, IBridge} from "../interfaces/IDriver.sol";
import {
    IStation, IStationFactory, IStationVault, Send, Stage, StationSpec
} from "../interfaces/IStation.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {IERC20Metadata} from "openzeppelin/token/ERC20/extensions/IERC20Metadata.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";

/// What the station has put into one venue, when dollars last moved between
/// them, and when a stopped unwind first closed through it at any price.
struct Flow {
    uint256 sent;
    uint64 depositedAt;
    uint64 movedAt;
    uint64 fullUnwindAt;
}

/// Everything a station keeps, in one place, so its own code and the
/// library its closes run in read and write the same state.
struct StationLayout {
    IStationFactory factory;
    StationSpec spec;
    bool stopped;
    uint64 stoppedAt;
    mapping(bytes32 => address) approvedKey;
    Send[] sends;
    mapping(bytes32 => address) pinnedDriver;
    mapping(bytes32 => address) pinnedBridge;
    /// The close in progress through delayed drivers, if any.
    Stage stage;
    /// What each delayed driver's account held when the stage was placed,
    /// in positions and in dollars, and the worth to sweep home from it.
    mapping(bytes32 => int256[]) stagedExposure;
    mapping(bytes32 => int256) stagedLedger;
    mapping(bytes32 => uint256) stagedFreed;
    /// Money found in a venue's own vaults through a driver, counted in the
    /// station's value and brought back as the vault's lock allows.
    mapping(bytes32 => address[]) held;
    /// A driver whose key parked money where the station had to reclaim it
    /// trades no more; closing and valuing through it still work.
    mapping(bytes32 => bool) reclaimed;
    /// Dollars that reached the station outside its own calls, counted up to
    /// the start of its current or last call, and its balance at the end of
    /// that call. Its balance only falls inside its own calls, so whatever it
    /// gained between them arrived from outside.
    uint256 arrived;
    uint256 seen;
    mapping(bytes32 => Flow) flows;
}

/// The station's storage, and the steps its own code and its closes share.
library StationStorage {
    using SafeERC20 for IERC20;

    /// keccak256(abi.encode(uint256(keccak256("shonin.station")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant SLOT = 0x6f766f5e802164a02b9d82ae8f2d3c1bb236fe4447b61828eddd5683735f2d00;
    bytes4 private constant APPROVE = IERC20.approve.selector;

    error NotDeclared();
    error BadCall();
    error NoRoute();
    error NotApproved();
    error AlreadyStopped();

    function layout() internal pure returns (StationLayout storage s) {
        bytes32 slot = SLOT;
        assembly ("memory-safe") {
            s.slot := slot
        }
    }

    function isHome(StationLayout storage s) internal view returns (bool) {
        return s.spec.vaultChain == block.chainid;
    }

    /// The implementation pinned at creation for a declared driver.
    function pinned(StationLayout storage s, bytes32 driver) internal view returns (address implementation) {
        implementation = s.pinnedDriver[driver];
        if (implementation == address(0)) revert NotDeclared();
    }

    function registry(StationLayout storage s) internal view returns (IAttestationRegistry) {
        return IAttestationRegistry(s.factory.attestation());
    }

    function asset(StationLayout storage s) internal view returns (IERC20) {
        return IERC20(s.factory.asset());
    }

    /// One dollar, in the asset's units.
    function oneDollar(StationLayout storage s) internal view returns (uint256) {
        return 10 ** IERC20Metadata(address(asset(s))).decimals();
    }

    function balance(StationLayout storage s) internal view returns (uint256) {
        return asset(s).balanceOf(address(this));
    }

    /// Make each call after checking where it goes: to one of the driver's
    /// targets, or to the dollar only to approve one of them.
    /// Nothing a driver prepares reaches the vault, the station itself, its
    /// factory or the registry, as a target or as a spender, whatever the
    /// driver lists; a call the station would refuse is never made.
    function run(StationLayout storage s, Call[] memory calls, address[] memory targets) internal {
        address dollar = address(asset(s));
        for (uint256 i = 0; i < calls.length; i++) {
            Call memory c = calls[i];
            if (!allowed(s, c, targets, dollar)) revert BadCall();
            (bool ok, bytes memory reason) = c.target.call{value: c.value}(c.data);
            if (!ok) {
                assembly {
                    revert(add(reason, 32), mload(reason))
                }
            }
        }
    }

    /// Whether the station makes `c` for a driver listing `targets`.
    function allowed(
        StationLayout storage s,
        Call memory c,
        address[] memory targets,
        address dollar
    ) internal view returns (bool) {
        if (c.target != dollar) return includes(targets, c.target) && !ours(s, c.target);
        if (c.data.length != 68 || bytes4(c.data) != APPROVE) return false;
        // The spender: the first argument, after the selector.
        bytes memory data = c.data;
        uint256 word;
        assembly ("memory-safe") {
            word := mload(add(data, 36))
        }
        // Not an address in its word: no approval anyone checked.
        address spender = address(uint160(word));
        return word >> 160 == 0 && includes(targets, spender) && !ours(s, spender);
    }

    function ours(StationLayout storage s, address target) internal view returns (bool) {
        return target == s.spec.vault || target == address(this) || target == address(s.factory)
            || target == address(registry(s));
    }

    /// Send `amount` of idle dollars to the vault's chain.
    function home(StationLayout storage s, uint256 amount) internal {
        if (isHome(s)) {
            sendHome(s, amount);
        } else if (s.spec.bridges.length != 0) {
            send(s, s.spec.bridges[0], s.spec.vaultChain, amount, "");
        } else {
            revert NoRoute();
        }
    }

    function sendHome(StationLayout storage s, uint256 amount) internal {
        asset(s).forceApprove(s.spec.vault, amount);
        IStationVault(s.spec.vault).fromStation(amount);
    }

    function send(
        StationLayout storage s,
        bytes32 name,
        uint256 toChain,
        uint256 amount,
        bytes memory extra
    ) internal {
        if (s.stopped && toChain != s.spec.vaultChain) revert AlreadyStopped();
        // Only to a chain the vault trades on, where a station counts it; and
        // a station on another chain sends only home, so money between two
        // other chains goes through home, where every send is counted
        // against one moment, the valuation's open.
        if (toChain != s.spec.vaultChain && (!isHome(s) || !listed(s.spec.chains, toChain))) revert NoRoute();
        if (!contains(s.spec.bridges, name)) revert NotDeclared();
        address implementation = s.pinnedBridge[name];
        if (registry(s).isBridgeRevoked(name, implementation)) revert NotApproved();
        // The recipient is this station's own address, which is the vault's
        // station on every chain, so a send can only reach the vault's own.
        run(
            s,
            IBridge(implementation).send(address(this), toChain, address(this), amount, extra),
            IBridge(implementation).targets()
        );
        s.sends.push(Send({toChain: toChain, amount: amount, sentAt: uint64(block.timestamp)}));
        // Numbered from zero, as `sendAt` and a post's in-flight list name it.
        emit IStation.Bridged(name, s.sends.length - 1, toChain, amount);
    }

    function listed(uint256[] storage list, uint256 item) internal view returns (bool) {
        for (uint256 i = 0; i < list.length; i++) {
            if (list[i] == item) return true;
        }
        return false;
    }

    function contains(bytes32[] storage list, bytes32 item) internal view returns (bool) {
        for (uint256 i = 0; i < list.length; i++) {
            if (list[i] == item) return true;
        }
        return false;
    }

    function includes(address[] memory list, address item) internal pure returns (bool) {
        for (uint256 i = 0; i < list.length; i++) {
            if (list[i] == item) return true;
        }
        return false;
    }
}
