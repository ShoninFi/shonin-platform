// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Allowances} from "../../../../../contracts/evm/src/interfaces/IPrices.sol";
import {ICloser} from "../../../../../contracts/evm/src/interfaces/ICloser.sol";
import {IStation} from "../../../../../contracts/evm/src/interfaces/IStation.sol";
import {ChainName, IVaultFactory, VaultConfig} from "../../../../../contracts/evm/src/interfaces/IVaultFactory.sol";
import {Scripted} from "./Scripted.sol";

/// A vault factory as the keeper lists vaults through it, for the keeper's
/// differential tests: a vault is listed from its scripted time on.
///
/// Script keys: `vaultCount` (how many of the vaults are listed),
/// `vault`/i, `previous` (the factory this one replaces), `attestation`
/// (the registry, as a station factory names it: the stations name this
/// factory as theirs too).
contract StandInFactory is Scripted, IVaultFactory {
    function vaults() external view returns (address[] memory listed) {
        uint256 count = _get("vaultCount");
        listed = new address[](count);
        for (uint256 i = 0; i < count; i++) {
            listed[i] = _addr(_get("vault", i));
        }
    }

    /// The factory this one replaces; not in the factory's shared interface.
    function previous() external view returns (address) {
        return _addr(_get("previous"));
    }

    /// The attestation registry, as a station factory names it.
    function attestation() external view returns (address) {
        return _addr(_get("attestation"));
    }

    function vaultOf(uint256) external pure returns (address) {}

    function predictVault(uint256) external pure returns (address) {}

    function restrictedFor(uint256) external pure returns (bytes32[] memory, string memory) {}

    function terms() external pure returns (bytes32) {}

    function config() external pure returns (VaultConfig memory) {}

    function chains() external pure returns (ChainName[] memory) {}

    function admin() external pure returns (address) {}

    function platform() external pure returns (address) {}

    function activate(uint256, Allowances calldata, uint16, uint256, uint256, bytes calldata, bytes32)
        external
        pure
        returns (address)
    {
        revert NotScripted();
    }

    function setConfig(VaultConfig calldata) external pure {
        revert NotScripted();
    }
}

/// What the stand-in Closer needs of a stand-in vault.
interface ICloseByCloser {
    function closerMayClose() external view returns (bool);
    function closeByCloser() external;
    function homeStation() external view returns (address);
}

/// The Closer, for the keeper's differential tests: for a vault whose script
/// lets it, stop and empty its station and close it, in one transaction.
contract StandInCloser is ICloser {
    error NotClosable();

    function closeHalted(address vault) external {
        ICloseByCloser closing = ICloseByCloser(vault);
        if (!closing.closerMayClose()) revert NotClosable();
        IStation station = IStation(closing.homeStation());
        if (!station.stopped()) station.stop();
        (uint256 mark,) = station.value();
        if (mark != 0) station.unwind(1e18);
        closing.closeByCloser();
    }
}
