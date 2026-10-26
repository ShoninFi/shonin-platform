// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IModuleProxyFactory, ISafeProxyFactory} from "../../src/drivers/robinhood/RobinhoodChain.sol";
import {RobinhoodDriver} from "../../src/drivers/robinhood/RobinhoodDriver.sol";
import {MockSafeFactory, MockToken} from "./AStockCloseRoundsByUnderAUnitPerFigure.t.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// The Robinhood driver's restricted list is the codes its venue's terms
/// exclude, as `config/drivers.toml` names them for `robinhood` and
/// `uniswap-base`: sorted, without repeats, and within what a vault reads
/// again, in codes and in gas.
contract TheRobinhoodDriverRestrictsWhatItsVenuesTermsExcludeTest is Test {
    /// What a vault lets a driver spend, and how many codes it takes, when
    /// it reads the list again.
    uint256 internal constant RESTRICTED_GAS = 200_000;
    uint256 internal constant MAX_RESTRICTED = 64;

    RobinhoodDriver internal driver;

    function setUp() public {
        driver = new RobinhoodDriver(
            RobinhoodDriver.Venue({
                dollar: IERC20(address(new MockToken(6))),
                router: makeAddr("router"),
                safeSingleton: makeAddr("singleton"),
                safeFactory: ISafeProxyFactory(address(new MockSafeFactory())),
                rolesMastercopy: makeAddr("roles"),
                moduleFactory: IModuleProxyFactory(makeAddr("moduleFactory")),
                maxFeedAge: 4 days,
                twapWindow: 1800
            }),
            new RobinhoodDriver.Listing[](0)
        );
    }

    function test_the_list_is_the_codes_the_catalogue_names_in_order() public view {
        bytes32[16] memory expected = [
            bytes32("BY"),
            "CA",
            "CH",
            "CU",
            "GB",
            "IR",
            "KP",
            "MM",
            "RU",
            "SANCTIONED",
            "SD",
            "SS",
            "SY",
            "UA",
            "US",
            "VE"
        ];
        bytes32[] memory list = driver.restricted();
        assertEq(list.length, expected.length);
        for (uint256 i = 0; i < list.length; i++) {
            assertEq(list[i], expected[i]);
            if (i > 0) assertGt(uint256(list[i]), uint256(list[i - 1]), "sorted, without repeats");
        }
    }

    function test_the_list_fits_what_a_vault_reads_again() public view {
        uint256 before = gasleft();
        bytes32[] memory list = driver.restricted{gas: RESTRICTED_GAS}();
        assertLt(before - gasleft(), RESTRICTED_GAS);
        assertLe(list.length, MAX_RESTRICTED);
    }
}
