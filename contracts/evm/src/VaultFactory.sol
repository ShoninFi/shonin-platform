// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "./Vault.sol";
import {IAttestationRegistry} from "./interfaces/IAttestationRegistry.sol";
import {ILocalVenue} from "./interfaces/ILocalVenue.sol";
import {Allowances, IPrices} from "./interfaces/IPrices.sol";
import {IReviewRegistry} from "./interfaces/IReviewRegistry.sol";
import {IStrategyRegistry, Strategy} from "./interfaces/IStrategyRegistry.sol";
import {Fees} from "./interfaces/IVault.sol";
import {IVaultFactory, VaultConfig} from "./interfaces/IVaultFactory.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";

/// One vault per strategy, created when its author activates it.
///
/// Publishing and activating are separate on purpose: the author reads the
/// review before putting any money in. A strategy that was refused or failed
/// to build has no record, so it can never have a vault.
contract VaultFactory is IVaultFactory {
    using SafeERC20 for IERC20;

    IERC20 public immutable asset;
    ILocalVenue public immutable venue;
    IAttestationRegistry public immutable attestation;
    IStrategyRegistry public immutable strategies;
    IReviewRegistry public immutable reviews;
    IPrices public immutable prices;
    address public immutable admin;
    address public immutable platform;

    VaultConfig private _config;
    mapping(uint256 => address) private _vaultOf;
    address[] private _vaults;

    error NotAdmin();
    error NotTheAuthor();
    error NotReady();
    error VaultExists();
    error TankTooSmall();
    error OutOfRange();

    uint256 private constant BPS = 10_000;

    // Ceilings on the terms, so no change of config can make a fee take what
    // it was never described as taking.
    uint256 public constant MAX_HOLDING_BPS = 500;
    uint256 public constant MAX_PROFIT_BPS = 5000;
    uint256 public constant MAX_TRANSACTION_BPS = 100;
    uint256 public constant MAX_EXIT_BAND_BPS = 1000;

    struct Deps {
        IERC20 asset;
        ILocalVenue venue;
        IAttestationRegistry attestation;
        IStrategyRegistry strategies;
        IReviewRegistry reviews;
        IPrices prices;
        address admin;
        address platform;
    }

    constructor(Deps memory deps, VaultConfig memory config_) {
        asset = deps.asset;
        venue = deps.venue;
        attestation = deps.attestation;
        strategies = deps.strategies;
        reviews = deps.reviews;
        prices = deps.prices;
        admin = deps.admin;
        platform = deps.platform;
        _checked(config_);
        _config = config_;
    }

    function activate(
        uint256 strategyId,
        Allowances calldata allowances,
        uint256 tankFunding
    ) external returns (address) {
        if (_vaultOf[strategyId] != address(0)) revert VaultExists();
        Strategy memory record = strategies.strategyOf(strategyId);
        if (record.registeredAt == 0 || !reviews.hasReview(strategyId)) revert NotReady();
        if (msg.sender != record.author) revert NotTheAuthor();
        if (tankFunding < _config.minimumTank) revert TankTooSmall();

        uint256 dailyCap = prices.dailyCost(allowances);
        Vault vault = new Vault(
            Vault.Init({
                asset: asset,
                venue: venue,
                attestation: attestation,
                platform: platform,
                admin: admin,
                author: msg.sender,
                strategyId: strategyId,
                bundleHash: record.bundleHash,
                sourceHash: record.sourceHash,
                fees: Fees({
                    streamingBps: _config.streamingBps,
                    authorBps: _config.authorBps,
                    platformBps: _config.platformBps,
                    transactionBps: _config.transactionBps
                }),
                exitBandBps: _config.exitBandBps,
                freezeAfter: _config.freezeAfter,
                dailyUsageCap: dailyCap,
                tankTarget: dailyCap * _config.tankTargetDays,
                autofillBps: _config.defaultAutofillBps
            })
        );

        _vaultOf[strategyId] = address(vault);
        _vaults.push(address(vault));

        asset.safeTransferFrom(msg.sender, address(this), tankFunding);
        asset.forceApprove(address(vault), tankFunding);
        vault.fundTank(tankFunding);

        emit Activated(strategyId, msg.sender, address(vault), tankFunding);
        return address(vault);
    }

    function vaultOf(uint256 strategyId) external view returns (address) {
        return _vaultOf[strategyId];
    }

    function vaults() external view returns (address[] memory) {
        return _vaults;
    }

    function config() external view returns (VaultConfig memory) {
        return _config;
    }

    function setConfig(VaultConfig calldata newConfig) external {
        if (msg.sender != admin) revert NotAdmin();
        _checked(newConfig);
        _config = newConfig;
        emit ConfigSet();
    }

    function _checked(VaultConfig memory config_) private pure {
        if (
            config_.streamingBps > MAX_HOLDING_BPS || config_.transactionBps > MAX_TRANSACTION_BPS
                || config_.exitBandBps > MAX_EXIT_BAND_BPS || config_.defaultAutofillBps > BPS
                || uint256(config_.authorBps) + config_.platformBps > MAX_PROFIT_BPS
                || config_.freezeAfter < 1 days || config_.tankTargetDays == 0 || config_.minimumTank == 0
        ) revert OutOfRange();
    }
}
