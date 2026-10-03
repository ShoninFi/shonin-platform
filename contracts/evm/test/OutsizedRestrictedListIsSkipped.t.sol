// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {Vault} from "../src/Vault.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A driver's list of who may not deposit is read again only up to its
/// bound of 64 codes: a driver that answers with a longer one is skipped
/// whole, so it can neither grow the vault's list without limit nor make
/// every depositor sign again. A list at the bound is still read.
contract OutsizedRestrictedListIsSkippedTest is Fixture {
    Vault internal vault;

    function setUp() public override {
        super.setUp();
        (vault,) = openVault();
    }

    function approve(uint256 codes) internal {
        ListingDriver listing = new ListingDriver(new LocalDriver(venue, IERC20(address(usd))), codes);
        vm.prank(admin);
        attestation.proposeDriver("local", address(listing));
        skip(TIMELOCK);
    }

    function test_a_list_of_sixty_five_codes_is_skipped_and_nobody_signs_again() public {
        depositAs(vault, alice, 1000e6);
        string memory before = vault.restrictedText();
        approve(65);
        vault.refreshRestricted();
        assertEq(vault.restrictedText(), before, "an outsized list was read");
        // Alice's statement still stands: she deposits without signing again.
        give(alice, 100e6);
        vm.startPrank(alice);
        usd.approve(address(vault), 100e6);
        vault.requestDeposit(100e6, alice, "");
        vm.stopPrank();
    }

    function test_a_list_of_sixty_four_codes_is_read() public {
        string memory before = vault.restrictedText();
        approve(64);
        vault.refreshRestricted();
        assertTrue(
            keccak256(bytes(vault.restrictedText())) != keccak256(bytes(before)),
            "a list at the bound was skipped"
        );
    }
}

/// The stand-in venue's driver, whose terms name `codes` codes.
contract ListingDriver is IDriver {
    LocalDriver internal immutable honest;
    uint256 internal immutable codes;

    constructor(LocalDriver honest_, uint256 codes_) {
        honest = honest_;
        codes = codes_;
    }

    function name() external pure returns (bytes32) {
        return "local";
    }

    function targets() external view returns (address[] memory) {
        return honest.targets();
    }

    function restricted() external view returns (bytes32[] memory list) {
        list = new bytes32[](codes);
        for (uint256 i = 0; i < codes; i++) {
            list[i] = bytes32(abi.encodePacked("C", bytes1(uint8(65 + i / 26)), bytes1(uint8(65 + i % 26))));
        }
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

    function approveKey(address station, address key, uint64 until) external view returns (Call[] memory) {
        return honest.approveKey(station, key, until);
    }

    function revokeKey(address station, address key) external view returns (Call[] memory) {
        return honest.revokeKey(station, key);
    }

    function deposit(address station, uint256 amount) external view returns (Call[] memory) {
        return honest.deposit(station, amount);
    }

    function withdraw(address station, uint256 amount) external view returns (Call[] memory) {
        return honest.withdraw(station, amount);
    }

    function close(
        address station,
        uint256 fraction,
        uint16 band
    ) external view returns (Call[] memory, bool) {
        return honest.close(station, fraction, band);
    }

    function sweep(address station, uint256 amount) external view returns (Call[] memory) {
        return honest.sweep(station, amount);
    }

    function valueOf(address station) external view returns (uint256, uint256) {
        return honest.valueOf(station);
    }
}
