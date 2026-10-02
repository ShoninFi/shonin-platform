// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Allowances} from "./IPrices.sol";
import {Timers} from "./IVault.sol";

/// The terms every new vault is created with. Changing them never touches a
/// vault that already exists.
struct VaultConfig {
    uint16 holdingBps;
    uint16 authorBps;
    uint16 platformBps;
    uint16 transactionBps;
    /// How far from the venue's own price a leaver's close may fill.
    uint16 exitBandBps;
    uint256 minimumDeposit;
    /// The least an author may activate a vault with, so an agent that can't
    /// run is never activated.
    uint256 minimumTank;
    /// What becomes the dead shares nobody can redeem.
    uint256 deadAssets;
    /// The most an unwinder may report as one claim's exit cost.
    uint256 maxExitCost;
    Timers timers;
}

/// A chain the platform runs on: its name as `shonin.toml` writes it, and its
/// chain id.
struct ChainName {
    bytes32 name;
    uint256 chainId;
}

/// One vault per strategy. Activating a built and reviewed strategy is what
/// creates it, from the strategy's own record, so its author never chooses
/// which code or which chains it runs.
interface IVaultFactory {
    event Activated(
        uint256 indexed strategyId,
        address indexed author,
        address vault,
        address station,
        uint256 tankFunding
    );
    event ConfigSet();

    /// The strategy's author only, once it is registered and reviewed: create
    /// its vault and its home station, on the terms the author read.
    function activate(
        uint256 strategyId,
        Allowances calldata allowances,
        uint16 idleFloorBps,
        uint256 tankFunding,
        uint256 tankKeep,
        bytes calldata statement,
        bytes32 terms
    ) external returns (address vault);

    /// Where the strategy's vault is, or will be.
    function predictVault(uint256 strategyId) external view returns (address);
    /// Who its vault will refuse, and the list as the author's statement
    /// carries it.
    function restrictedFor(uint256 strategyId)
        external
        view
        returns (bytes32[] memory list, string memory text);
    /// The hash an author passes to `activate`: the terms in force.
    function terms() external view returns (bytes32);

    function vaultOf(uint256 strategyId) external view returns (address);
    function vaults() external view returns (address[] memory);
    function config() external view returns (VaultConfig memory);
    function chains() external view returns (ChainName[] memory);
    /// Admin only.
    function setConfig(VaultConfig calldata newConfig) external;
    function admin() external view returns (address);
    function platform() external view returns (address);
}
