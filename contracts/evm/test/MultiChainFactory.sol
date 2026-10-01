// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {VaultFactory} from "../src/VaultFactory.sol";
import {ChainName, VaultConfig} from "../src/interfaces/IVaultFactory.sol";

/// A factory whose vaults may trade on other chains, so the vault's
/// cross-chain pricing can be tested before a factory that ships allows it.
contract MultiChainFactory is VaultFactory {
    constructor(
        Deps memory deps,
        VaultConfig memory config,
        ChainName[] memory chains
    ) VaultFactory(deps, config, chains) {}

    function _tradesOnOtherChains() internal pure override returns (bool) {
        return true;
    }
}
