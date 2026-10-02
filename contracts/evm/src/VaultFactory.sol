// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry} from "./interfaces/IAttestationRegistry.sol";
import {IDriver} from "./interfaces/IDriver.sol";
import {Allowances, IPrices} from "./interfaces/IPrices.sol";
import {IReviewRegistry} from "./interfaces/IReviewRegistry.sol";
import {IStationFactory} from "./interfaces/IStation.sol";
import {ChainNeed, IStrategyRegistry, Strategy} from "./interfaces/IStrategyRegistry.sol";
import {Fees, IVault, VaultInit} from "./interfaces/IVault.sol";
import {ChainName, IVaultFactory, VaultConfig} from "./interfaces/IVaultFactory.sol";
import {Codes} from "./vault/Codes.sol";

import {Clones} from "openzeppelin/proxy/Clones.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";

/// One vault per strategy, created when its author activates it.
///
/// Publishing and activating are separate on purpose: the author reads the
/// review before putting any money in. A strategy that was refused or failed
/// to build has no record, so it can never have a vault. Each vault is a
/// clone at an address fixed by its strategy id, so its author can sign the
/// depositor's statement for it before it exists.
contract VaultFactory is IVaultFactory {
    using SafeERC20 for IERC20;

    uint256 private constant BPS = 10_000;

    // Ceilings on the terms, so no change of config can make a fee take what
    // it was never described as taking.
    uint256 public constant MAX_HOLDING_BPS = 500;
    uint256 public constant MAX_PROFIT_BPS = 5000;
    uint256 public constant MAX_TRANSACTION_BPS = 100;
    uint256 public constant MAX_EXIT_BAND_BPS = 1000;
    /// The most an unwinder may charge one claim for bringing it home: a
    /// thousand of the vault's six-decimal dollar.
    uint256 public constant MAX_EXIT_COST = 1000e6;

    IERC20 public immutable asset;
    IAttestationRegistry public immutable attestation;
    IStrategyRegistry public immutable strategies;
    IReviewRegistry public immutable reviews;
    IPrices public immutable prices;
    IStationFactory public immutable stationFactory;
    address public immutable vaultImplementation;
    address public immutable admin;
    address public immutable platform;

    VaultConfig private _config;
    ChainName[] private _chains;
    mapping(uint256 => address) private _vaultOf;
    address[] private _vaults;

    error NotAdmin();
    error NotTheAuthor();
    error NotReady();
    error VaultExists();
    error TankTooSmall();
    error OutOfRange();
    error TermsChanged();
    error UnknownChain(bytes32 name);
    error UnknownDriver();
    error ChainTwice();
    error OtherChainsNotYet();

    struct Deps {
        IERC20 asset;
        IAttestationRegistry attestation;
        IStrategyRegistry strategies;
        IReviewRegistry reviews;
        IPrices prices;
        IStationFactory stationFactory;
        address vaultImplementation;
        address admin;
        address platform;
    }

    constructor(Deps memory deps, VaultConfig memory config_, ChainName[] memory chains_) {
        asset = deps.asset;
        attestation = deps.attestation;
        strategies = deps.strategies;
        reviews = deps.reviews;
        prices = deps.prices;
        stationFactory = deps.stationFactory;
        vaultImplementation = deps.vaultImplementation;
        admin = deps.admin;
        platform = deps.platform;
        _checked(config_);
        _config = config_;
        for (uint256 i = 0; i < chains_.length; i++) {
            _chains.push(chains_[i]);
        }
    }

    function activate(
        uint256 strategyId,
        Allowances calldata allowances,
        uint16 idleFloorBps,
        uint256 tankFunding,
        uint256 tankKeep,
        bytes calldata statement,
        bytes32 terms_
    ) external returns (address vault) {
        if (_vaultOf[strategyId] != address(0)) revert VaultExists();
        // The author is held to what they read: a change of terms between
        // reading and activating stops the activation.
        if (terms_ != terms()) revert TermsChanged();
        Strategy memory record = strategies.strategyOf(strategyId);
        if (record.registeredAt == 0 || !reviews.hasReview(strategyId)) revert NotReady();
        if (msg.sender != record.author) revert NotTheAuthor();
        VaultConfig memory cfg = _config;
        if (tankFunding < cfg.minimumTank) revert TankTooSmall();
        if (idleFloorBps > BPS) revert OutOfRange();

        (bytes32[] memory drivers, uint256[] memory others) = _chainsOf(record);
        if (others.length != 0 && !_tradesOnOtherChains()) revert OtherChainsNotYet();
        vault = Clones.cloneDeterministic(vaultImplementation, bytes32(strategyId));
        _vaultOf[strategyId] = vault;
        _vaults.push(vault);
        address station = stationFactory.createHome(
            vault,
            record.bundleHash,
            drivers,
            others.length == 0 ? new bytes32[](0) : record.bridges,
            others,
            cfg.exitBandBps
        );

        asset.safeTransferFrom(msg.sender, vault, tankFunding);
        IVault(vault).initialize(
            VaultInit({
                asset: address(asset),
                attestation: address(attestation),
                prices: address(prices),
                homeStation: station,
                platform: platform,
                admin: admin,
                author: msg.sender,
                strategyId: strategyId,
                bundleHash: record.bundleHash,
                fees: Fees({
                    holdingBps: cfg.holdingBps,
                    authorBps: cfg.authorBps,
                    platformBps: cfg.platformBps,
                    transactionBps: cfg.transactionBps
                }),
                exitBandBps: cfg.exitBandBps,
                idleFloorBps: idleFloorBps,
                minimumDeposit: cfg.minimumDeposit,
                timers: cfg.timers,
                allowances: allowances,
                tankKeep: tankKeep,
                restricted: _restricted(record, drivers),
                otherChains: others,
                maxExitCost: cfg.maxExitCost
            }),
            tankFunding,
            cfg.deadAssets
        );
        // The author is the first holder, and signs like every other.
        IVault(vault).recordStatement(msg.sender, statement);
        emit Activated(strategyId, msg.sender, vault, station, tankFunding);
    }

    function predictVault(uint256 strategyId) external view returns (address) {
        return Clones.predictDeterministicAddress(vaultImplementation, bytes32(strategyId));
    }

    function restrictedFor(uint256 strategyId)
        external
        view
        returns (bytes32[] memory list, string memory text)
    {
        Strategy memory record = strategies.strategyOf(strategyId);
        (bytes32[] memory drivers,) = _chainsOf(record);
        list = _restricted(record, drivers);
        text = Codes.text(list);
    }

    function terms() public view returns (bytes32) {
        return keccak256(abi.encode(_config));
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

    function chains() external view returns (ChainName[] memory) {
        return _chains;
    }

    function setConfig(VaultConfig calldata newConfig) external {
        if (msg.sender != admin) revert NotAdmin();
        _checked(newConfig);
        _config = newConfig;
        emit ConfigSet();
    }

    /// This chain's drivers, and the other chains it trades on, by id.
    /// Whether a vault may trade on chains besides its own. Not from this
    /// factory: a leaver's share on another chain can only be brought home
    /// by an unwinder working there, so a strategy that needs one waits for
    /// the factory that comes with it.
    function _tradesOnOtherChains() internal pure virtual returns (bool) {
        return false;
    }

    function _chainsOf(Strategy memory record)
        private
        view
        returns (bytes32[] memory drivers, uint256[] memory others)
    {
        others = new uint256[](record.chains.length);
        uint256 count;
        for (uint256 i = 0; i < record.chains.length; i++) {
            ChainNeed memory need = record.chains[i];
            uint256 id = _idOf(need.chain);
            // A chain named twice would wait for a post that never comes.
            for (uint256 j = 0; j < i; j++) {
                if (_idOf(record.chains[j].chain) == id) revert ChainTwice();
            }
            if (id == block.chainid) {
                drivers = need.drivers;
            } else {
                others[count++] = id;
            }
        }
        assembly {
            mstore(others, count)
        }
    }

    function _idOf(bytes32 name) private view returns (uint256) {
        for (uint256 i = 0; i < _chains.length; i++) {
            if (_chains[i].name == name) return _chains[i].chainId;
        }
        revert UnknownChain(name);
    }

    /// Who the vault refuses: US and sanctioned persons, every driver's list
    /// on every chain as the registry recorded it with the author's
    /// additions, and this chain's drivers as they stand now.
    function _restricted(
        Strategy memory record,
        bytes32[] memory drivers
    ) private view returns (bytes32[] memory list) {
        list = Codes.with(Codes.with(new bytes32[](0), "SANCTIONED"), "US");
        for (uint256 i = 0; i < record.restricted.length; i++) {
            list = Codes.with(list, record.restricted[i]);
        }
        for (uint256 i = 0; i < drivers.length; i++) {
            address implementation = attestation.unwindDriver(drivers[i]);
            // A driver nobody approved could never value or stop the station.
            if (implementation == address(0)) revert UnknownDriver();
            bytes32[] memory codes = IDriver(implementation).restricted();
            for (uint256 j = 0; j < codes.length; j++) {
                list = Codes.with(list, codes[j]);
            }
        }
    }

    function _checked(VaultConfig memory config_) private pure {
        if (
            config_.holdingBps > MAX_HOLDING_BPS || config_.transactionBps > MAX_TRANSACTION_BPS
                || config_.exitBandBps > MAX_EXIT_BAND_BPS
                || uint256(config_.authorBps) + config_.platformBps > MAX_PROFIT_BPS || config_.deadAssets == 0
                || config_.minimumTank <= config_.deadAssets || config_.minimumDeposit == 0
                || config_.timers.slotInterval == 0 || config_.timers.slotWindow >= config_.timers.slotInterval
                || config_.timers.slotLead <= config_.timers.slotWindow || config_.timers.freezeAfter < 1 days
                || config_.timers.fallbackAfter < 1 days
                || config_.timers.postDeadline <= config_.timers.slotWindow || config_.maxExitCost > MAX_EXIT_COST
        ) revert OutOfRange();
    }
}
