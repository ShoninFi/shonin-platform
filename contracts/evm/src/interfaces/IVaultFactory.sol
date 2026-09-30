// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

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
    /// The least an author may create a vault with: the build, the review and
    /// a minimum run.
    uint256 minimumTank;
}

/// One vault per strategy version. Creating it is how a strategy is published:
/// the author funds the tank in the same transaction, and the build and the
/// review are its first charges.
interface IVaultFactory {
    event VaultCreated(
        bytes32 indexed strategyId,
        uint32 indexed version,
        address indexed author,
        address vault,
        uint256 tankFunding
    );
    event ConfigSet();

    function create(
        bytes32 strategyId,
        uint32 version,
        bytes32 sourceHash,
        uint256 tankFunding,
        uint256 dailyUsageCap
    ) external returns (address vault);

    function vaultOf(bytes32 strategyId, uint32 version) external view returns (address);
    function vaults() external view returns (address[] memory);
    function config() external view returns (VaultConfig memory);
    /// Admin only.
    function setConfig(VaultConfig calldata newConfig) external;
    function admin() external view returns (address);
    function platform() external view returns (address);
}
