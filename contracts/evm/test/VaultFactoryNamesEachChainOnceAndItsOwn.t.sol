// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {VaultFactory} from "../src/VaultFactory.sol";
import {ChainName} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A vault factory's chain list is fixed when it is created and is what
/// every strategy's chains are read against. Each chain is in it once, by
/// name and by id, and the chain the factory is on is among them: without
/// it, no strategy that trades here could ever be activated.
contract VaultFactoryNamesEachChainOnceAndItsOwnTest is Fixture {
    function _two(
        bytes32 a,
        uint256 idA,
        bytes32 b,
        uint256 idB
    ) private pure returns (ChainName[] memory list) {
        list = new ChainName[](2);
        list[0] = ChainName({name: a, chainId: idA});
        list[1] = ChainName({name: b, chainId: idB});
    }

    function test_a_chain_named_twice_is_refused() public {
        VaultFactory.Deps memory deps = _deps();
        vm.expectRevert(VaultFactory.ChainTwice.selector);
        newFactory(deps, vaultConfig(), _two("base", block.chainid, "base", 999));
    }

    function test_one_chain_under_two_names_is_refused() public {
        VaultFactory.Deps memory deps = _deps();
        vm.expectRevert(VaultFactory.ChainTwice.selector);
        newFactory(deps, vaultConfig(), _two("base", block.chainid, "base-again", block.chainid));
    }

    function test_a_list_without_the_chain_the_factory_is_on_is_refused() public {
        ChainName[] memory elsewhere = new ChainName[](1);
        elsewhere[0] = ChainName({name: "hyperliquid", chainId: 999});
        VaultFactory.Deps memory deps = _deps();
        vm.expectRevert(abi.encodeWithSelector(VaultFactory.UnknownChain.selector, bytes32(0)));
        newFactory(deps, vaultConfig(), elsewhere);
    }

    function test_a_chain_without_a_name_or_an_id_is_refused() public {
        VaultFactory.Deps memory deps = _deps();
        vm.expectRevert(VaultFactory.OutOfRange.selector);
        newFactory(deps, vaultConfig(), _two("base", block.chainid, "", 999));
        vm.expectRevert(VaultFactory.OutOfRange.selector);
        newFactory(deps, vaultConfig(), _two("base", block.chainid, "hyperliquid", 0));
    }

    function test_each_chain_once_with_its_own_is_taken() public {
        VaultFactory made =
            newFactory(_deps(), vaultConfig(), _two("base", block.chainid, "hyperliquid", 999));
        assertEq(made.chains().length, 2);
    }

    function _deps() private view returns (VaultFactory.Deps memory) {
        return VaultFactory.Deps({
            asset: IERC20(address(usd)),
            attestation: attestation,
            strategies: strategies,
            reviews: reviews,
            prices: prices,
            stationFactory: stations,
            vaultImplementation: factory.vaultImplementation(),
            admin: admin,
            platform: platform,
            previous: factory.previous()
        });
    }
}
