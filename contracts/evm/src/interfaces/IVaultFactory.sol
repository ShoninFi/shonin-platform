// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Allowances} from "./IPrices.sol";

/// The terms every new vault is created with. Changing them never touches a
/// vault that already exists.
struct VaultConfig {
    uint16 streamingBps;
    uint16 authorBps;
    uint16 platformBps;
    uint16 transactionBps;
    /// How far either side of the venue's oracle price a claim may close.
    uint16 exitBandBps;
    /// The default share of an author's fee that refills the tank.
    uint16 defaultAutofillBps;
    /// How long an unserved claim freezes the vault after.
    uint32 freezeAfter;
    /// Days of charges auto-fill aims to keep in the tank.
    uint32 tankTargetDays;
    /// The least an author may activate a vault with, so an agent that can't
    /// run is never activated.
    uint256 minimumTank;
}

/// One vault per strategy. Activating a built and reviewed strategy is what
/// creates it, from the strategy's own record, so its author never chooses
/// which code or which chains it runs.
interface IVaultFactory {
    event Activated(uint256 indexed strategyId, address indexed author, address vault, uint256 tankFunding);
    event ConfigSet();

    /// The strategy's author only, once it is registered and reviewed: create
    /// its vault, with the allowances its tank is charged against and a
    /// starting tank of at least the minimum.
    function activate(
        uint256 strategyId,
        Allowances calldata allowances,
        uint256 tankFunding
    ) external returns (address vault);

    function vaultOf(uint256 strategyId) external view returns (address);
    function vaults() external view returns (address[] memory);
    function config() external view returns (VaultConfig memory);
    /// Admin only.
    function setConfig(VaultConfig calldata newConfig) external;
    function admin() external view returns (address);
    function platform() external view returns (address);
}
