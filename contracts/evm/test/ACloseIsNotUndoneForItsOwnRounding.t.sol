// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Call, Custody, ExitPath, ValuationKind} from "../src/interfaces/IDriver.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A venue whose exit value sums several figures, each rounded down (idle
/// dollars, one quote per holding): after a close of a fraction its account
/// reads a few dollar-units above the stayers' share by rounding alone, and
/// its sale fetched a unit under the fraction's worth (a pool's fee).
contract RoundingVenueDriver {
    uint256 private constant WAD = 1e18;

    IERC20 public immutable asset;
    uint256 public exitValue;
    /// Dollar-units the account reads above the stayers' share after a close.
    uint256 public leftOver;

    constructor(IERC20 asset_) {
        asset = asset_;
    }

    function set(uint256 exitValue_, uint256 leftOver_) external {
        exitValue = exitValue_;
        leftOver = leftOver_;
    }

    function name() external pure returns (bytes32) {
        return "local";
    }

    function targets() external view returns (address[] memory list) {
        list = new address[](1);
        list[0] = address(this);
    }

    function restricted() external pure returns (bytes32[] memory) {
        return new bytes32[](0);
    }

    function custody() external pure returns (Custody) {
        return Custody.Venue;
    }

    function exitPath() external pure returns (ExitPath) {
        return ExitPath.Contract;
    }

    function valuation() external pure returns (ValuationKind) {
        return ValuationKind.OnChain;
    }

    function approveKey(address, address, uint64) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function revokeKey(address, address) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function deposit(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function withdraw(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function close(address, uint256 fraction, uint16) external view returns (Call[] memory calls, bool) {
        calls = new Call[](1);
        calls[0] = Call(address(this), 0, abi.encodeCall(this.fill, (fraction)));
    }

    function sweep(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function valueOf(address) external view returns (uint256, uint256) {
        return (exitValue, exitValue);
    }

    /// The sale: a unit under the fraction's worth reaches the station, and
    /// the account is left `leftOver` above the stayers' share.
    function fill(uint256 fraction) external {
        uint256 freed = exitValue * fraction / WAD;
        asset.transfer(msg.sender, freed - 1);
        exitValue = exitValue - freed + leftOver;
    }
}

/// A claim's close whose sale fetched a little under the fraction's worth is
/// undone only when the account is left worth more than the stayers' share:
/// proceeds kept at the venue are the leaver's. What the account's own
/// figures round by is not that, and doesn't undo a close: it stays with the
/// stayers, and the leaver is paid what the sale fetched.
contract ACloseIsNotUndoneForItsOwnRoundingTest is ClaimBase {
    function _claimOnARoundingVenue(uint256 leftOver) internal returns (uint256 claimId) {
        depositAs(vault, alice, 1000e6);
        uint256 bobShares = depositAs(vault, bob, 1000e6);
        pinnedDriverActsAs(address(new RoundingVenueDriver(IERC20(address(usd)))));
        RoundingVenueDriver venueDriver = RoundingVenueDriver(address(driver));
        // The account holds a thousand, all of it at the venue; the vault's
        // cash can't pay Bob.
        venueDriver.set(1000e6, leftOver);
        give(address(driver), 1000e6);
        vm.prank(executor);
        station.pull(1900e6);
        claimId = claimFor(bob, bobShares);
    }

    function test_a_close_left_a_few_units_above_the_stayers_share_by_rounding_pays_the_leaver() public {
        uint256 id = _claimOnARoundingVenue(3);
        vault.settleClaim(id);
        assertEq(uint256(vault.claimAt(id).state), uint256(ClaimState.Paid), "the close was undone");
    }

    function test_a_close_that_left_the_leavers_proceeds_at_the_venue_is_undone() public {
        uint256 id = _claimOnARoundingVenue(1000);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state != ClaimState.Paid, "the leaver was paid short");
    }
}
