// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IStation, IStationFactory, IStationVault} from "../../src/interfaces/IStation.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// The least a home station needs of its vault, for showing a station trade
/// on a real venue without the whole vault: it creates the station, asks it
/// to close a fraction as a claim would, and takes what comes home. Never
/// for real money: it has an owner and no accounting.
contract DemoVault is IStationVault {
    IERC20 public immutable asset;
    IStationFactory public immutable factory;
    address public immutable owner;
    address public station;
    uint256 public cash;

    event Home(uint256 amount);

    constructor(IERC20 asset_, IStationFactory factory_) {
        asset = asset_;
        factory = factory_;
        owner = msg.sender;
    }

    modifier onlyOwner() {
        require(msg.sender == owner, "owner only");
        _;
    }

    function open(bytes32 bundleHash, bytes32[] calldata drivers, uint16 exitBandBps) external onlyOwner {
        station = factory.createHome(
            address(this), bundleHash, drivers, new bytes32[](0), new uint256[](0), exitBandBps
        );
    }

    /// What a claim does: close `fraction` of every position.
    function claim(uint256 fraction) external onlyOwner returns (uint256 sentHome, bool blocked) {
        return IStation(station).unwind(fraction);
    }

    function toStation(uint256 amount) external {
        require(msg.sender == station, "station only");
        cash -= amount;
        asset.transfer(station, amount);
    }

    function fromStation(uint256 amount) external {
        require(msg.sender == station, "station only");
        asset.transferFrom(station, address(this), amount);
        cash += amount;
        emit Home(amount);
    }

    function haltedAt() external pure returns (uint64) {
        return 0;
    }

    function haltGrace() external pure returns (uint64) {
        return 0;
    }

    function valuing() external pure returns (bool) {
        return false;
    }

    function unsettled() external pure returns (bool) {
        return false;
    }
}
