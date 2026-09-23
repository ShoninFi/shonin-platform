// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "./Vault.sol";
import {IAttestationRegistry} from "./interfaces/IAttestationRegistry.sol";
import {ILocalVenue} from "./interfaces/ILocalVenue.sol";
import {IReviewRegistry} from "./interfaces/IReviewRegistry.sol";
import {IStrategyRegistry} from "./interfaces/IStrategyRegistry.sol";
import {Fees} from "./interfaces/IVault.sol";
import {IVaultFactory, VaultConfig} from "./interfaces/IVaultFactory.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";

/// One vault per strategy version.
///
/// Creating a vault is how a strategy is published: the author funds its tank
/// in the same transaction, and the build and the review are the first two
/// things that tank pays for. A vault without a funded tank never exists, so
/// nothing can be queued for free.
contract VaultFactory is IVaultFactory {
    using SafeERC20 for IERC20;

    IERC20 public immutable asset;
    ILocalVenue public immutable venue;
    IAttestationRegistry public immutable attestation;
    IStrategyRegistry public immutable strategies;
    IReviewRegistry public immutable reviews;
    address public immutable admin;
    address public immutable platform;

    VaultConfig private _config;
    mapping(bytes32 => mapping(uint32 => address)) private _vaultOf;
    address[] private _vaults;

    error NotAdmin();
    error VaultExists();
    error TankTooSmall();

    constructor(
        IERC20 asset_,
        ILocalVenue venue_,
        IAttestationRegistry attestation_,
        IStrategyRegistry strategies_,
        IReviewRegistry reviews_,
        address admin_,
        address platform_,
        VaultConfig memory config_
    ) {
        asset = asset_;
        venue = venue_;
        attestation = attestation_;
        strategies = strategies_;
        reviews = reviews_;
        admin = admin_;
        platform = platform_;
        _config = config_;
    }

    function create(
        bytes32 strategyId,
        uint32 version,
        bytes32 sourceHash,
        uint256 tankFunding,
        uint256 dailyUsageCap
    ) external returns (address) {
        if (_vaultOf[strategyId][version] != address(0)) revert VaultExists();
        if (tankFunding < _config.minimumTank) revert TankTooSmall();

        Vault vault = new Vault(
            Vault.Init({
                asset: asset,
                venue: venue,
                attestation: attestation,
                strategies: strategies,
                platform: platform,
                admin: admin,
                author: msg.sender,
                strategyId: strategyId,
                version: version,
                sourceHash: sourceHash,
                fees: Fees({
                    streamingBps: _config.streamingBps,
                    authorBps: _config.authorBps,
                    platformBps: _config.platformBps,
                    transactionBps: _config.transactionBps
                }),
                exitBandBps: _config.exitBandBps,
                freezeAfter: _config.freezeAfter,
                dailyUsageCap: dailyUsageCap,
                tankTarget: dailyUsageCap * _config.tankTargetDays,
                autofillBps: _config.defaultAutofillBps
            })
        );

        _vaultOf[strategyId][version] = address(vault);
        _vaults.push(address(vault));

        // The tank comes in whole, and the review is charged to it at once, so
        // a strategy is scored whether or not anyone asks.
        asset.safeTransferFrom(msg.sender, address(this), tankFunding);
        uint256 reviewFee = reviews.reviewFee();
        asset.forceApprove(address(reviews), reviewFee);
        reviews.requestReview(strategyId, version);

        uint256 rest = tankFunding - reviewFee;
        asset.forceApprove(address(vault), rest);
        vault.fundTank(rest);

        emit VaultCreated(strategyId, version, msg.sender, address(vault), tankFunding);
        return address(vault);
    }

    function vaultOf(bytes32 strategyId, uint32 version) external view returns (address) {
        return _vaultOf[strategyId][version];
    }

    function vaults() external view returns (address[] memory) {
        return _vaults;
    }

    function config() external view returns (VaultConfig memory) {
        return _config;
    }

    function setConfig(VaultConfig calldata newConfig) external {
        if (msg.sender != admin) revert NotAdmin();
        _config = newConfig;
        emit ConfigSet();
    }
}
