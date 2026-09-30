// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry} from "../src/interfaces/IAttestationRegistry.sol";
import {IAuthorProfiles} from "../src/interfaces/IAuthorProfiles.sol";
import {ILocalUsd} from "../src/interfaces/ILocalUsd.sol";
import {ILocalVenue} from "../src/interfaces/ILocalVenue.sol";
import {IReviewRegistry} from "../src/interfaces/IReviewRegistry.sol";
import {IStrategyRegistry} from "../src/interfaces/IStrategyRegistry.sol";
import {IVault} from "../src/interfaces/IVault.sol";
import {IVaultFactory} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";

/// The interfaces under `src/interfaces` are read twice: the contracts
/// implement them, and everything off-chain generates its bindings from the
/// same files. This suite calls each deployed contract *through* its interface,
/// so a signature that drifts apart from its implementation stops the build
/// here rather than at run time in a service.
contract InterfacesTest is Fixture {
    function test_the_venue_answers_to_its_interface() public view {
        ILocalVenue asInterface = ILocalVenue(address(venue));
        assertEq(asInterface.markets().length, 2);
        assertEq(asInterface.marketOf(SPOT).mark, PRICE);
        assertEq(asInterface.marginBps(), MARGIN_BPS);
        assertEq(asInterface.cashOf(address(this)), 0);
    }

    function test_the_registries_answer_to_their_interfaces() public view {
        assertEq(IAttestationRegistry(address(attestation)).timelock(), TIMELOCK);
        assertTrue(IAttestationRegistry(address(attestation)).isImageAllowed(BUILDER_IMAGE));
        assertEq(IStrategyRegistry(address(strategies)).bundleHashOf("nothing", 1), bytes32(0));
        assertEq(IReviewRegistry(address(reviews)).reviewFee(), REVIEW_FEE);
        assertEq(IAuthorProfiles(address(profiles)).authorOfHandle("nobody"), address(0));
    }

    function test_the_factory_and_its_vaults_answer_to_their_interfaces() public {
        IVaultFactory asFactory = IVaultFactory(address(factory));
        assertEq(asFactory.config().minimumTank, MINIMUM_TANK);
        assertEq(asFactory.vaults().length, 0);

        (, bytes32 bundle) = openVault("interfaces", 1);
        IVault asVault = IVault(asFactory.vaults()[0]);
        assertEq(asVault.strategyId(), "interfaces");
        assertEq(asVault.sharePrice(), 1e6);
        assertEq(asVault.fees().transactionBps, 10);
        assertEq(asVault.tank().balance, MINIMUM_TANK - REVIEW_FEE);
        assertEq(asVault.exitBandBps(), 100);
        assertEq(asVault.claimCount(), 0);
        assertTrue(bundle != bytes32(0));
    }

    function test_the_dollar_answers_to_its_interface() public {
        ILocalUsd asInterface = ILocalUsd(address(usd));
        asInterface.mint(address(this), 5e6);
        assertEq(asInterface.decimals(), 6);
        assertEq(asInterface.symbol(), "USDC");
        assertEq(asInterface.balanceOf(address(this)), 5e6);
        assertTrue(asInterface.transfer(alice, 1e6));
        assertTrue(asInterface.approve(alice, 1e6));
        assertEq(asInterface.allowance(address(this), alice), 1e6);
        assertEq(asInterface.totalSupply(), 5e6);
    }

    /// LocalUsd is an ordinary OpenZeppelin token, so nothing forces it to
    /// keep matching the interface the bindings read; this does.
    function test_the_dollar_is_the_token_the_interface_describes() public view {
        assertEq(ILocalUsd.mint.selector, usd.mint.selector);
        assertEq(ILocalUsd.decimals.selector, usd.decimals.selector);
        assertEq(ILocalUsd.transferFrom.selector, usd.transferFrom.selector);
        assertEq(ILocalUsd.balanceOf.selector, usd.balanceOf.selector);
    }
}
