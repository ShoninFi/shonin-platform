// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {LocalVenue} from "../src/LocalVenue.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A single-chain vault trading through two venues on its own chain, with an
/// idle floor of zero, as the dust tests set one up.
abstract contract TwoVenueVault is Fixture {
    uint256 internal constant WAD = 1e18;

    LocalVenue internal venue2;
    LocalDriver internal driver2;
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public virtual override {
        super.setUp();
        venue2 = new LocalVenue(usd, operator, MARGIN_BPS);
        driver2 = new LocalDriver(venue2, IERC20(address(usd)));
        vm.prank(admin);
        attestation.proposeDriver("local2", address(driver2));
        skip(TIMELOCK);
        vm.prank(operator);
        venue2.listMarket(SPOT, false, PRICE);
        give(address(venue), 10_000_000e6);
        give(address(venue2), 10_000_000e6);

        uint256 id = submitAs(author);
        bundle = keccak256(abi.encode("bundle", id));
        ChainNeed[] memory chains = new ChainNeed[](1);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](2);
        chains[0].drivers[0] = "local";
        chains[0].drivers[1] = "local2";
        bytes32[] memory none = new bytes32[](0);
        bytes32[] memory restricted = new bytes32[](2);
        restricted[0] = "SANCTIONED";
        restricted[1] = "US";
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), bundle, keccak256("manifest"), chains, none, restricted
        );
        strategies.register(
            id,
            author,
            keccak256("source"),
            bundle,
            keccak256("manifest"),
            chains,
            none,
            restricted,
            signed(BUILDER_KEY, digest),
            signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")))
        );
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(bundle, 1));

        (, string memory text) = factory.restrictedFor(id);
        bytes memory statement = statementBy(authorKey, factory.predictVault(id), author, text);
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vault = Vault(
            factory.activate(id, allowancesCosting(2e6), 0, MINIMUM_TANK, 0, statement, factory.terms())
        );
        vm.stopPrank();
        blessExecutorKey(executor, address(vault), bundle);
        station = stationOf(vault);
    }

    /// Buy `size` of spot at `v` for the station, at PRICE.
    function buySpot(LocalVenue v, uint256 size) internal {
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: size, limitPrice: PRICE, reduceOnly: false});
        vm.prank(executor);
        v.place(address(station), orders);
    }

    /// A symmetric spread of `spreadBps` around PRICE at `v`.
    function spread(LocalVenue v, uint256 spreadBps) internal {
        uint256 half = (PRICE * spreadBps) / 20_000;
        vm.prank(operator);
        v.setPrices(SPOT, PRICE, PRICE, PRICE - half, PRICE + half);
    }
}
