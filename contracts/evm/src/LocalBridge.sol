// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalUsd} from "./LocalUsd.sol";
import {Call, IBridge} from "./interfaces/IDriver.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";

/// The stand-in bridge, for local chains and testnets.
///
/// A send takes the dollars on this chain and records where they are going.
/// The operator delivers them on the other chain by minting the development
/// dollar, which is free there, once per send. It exists so the paths a real
/// bridge takes -- the recipient check, money in transit, its landing -- run
/// before a real bridge is wired in.
contract LocalBridge is IBridge {
    using SafeERC20 for IERC20;

    IERC20 public immutable asset;
    /// Delivers sends from other chains.
    address public immutable operator;

    uint256 public sent;
    mapping(uint256 => mapping(uint256 => bool)) public delivered;

    event Carried(
        uint256 indexed sequence,
        address indexed from,
        uint256 indexed toChain,
        address recipient,
        uint256 amount
    );
    event Delivered(
        uint256 indexed fromChain, uint256 indexed sequence, address indexed recipient, uint256 amount
    );

    error NotOperator();
    error AlreadyDelivered();

    constructor(IERC20 asset_, address operator_) {
        asset = asset_;
        operator = operator_;
    }

    function name() external pure returns (bytes32) {
        return "local-bridge";
    }

    function targets() external view returns (address[] memory list) {
        list = new address[](1);
        list[0] = address(this);
    }

    function send(
        address,
        uint256 toChain,
        address recipient,
        uint256 amount,
        bytes calldata
    ) external view returns (Call[] memory calls) {
        calls = new Call[](2);
        calls[0] = Call(address(asset), 0, abi.encodeCall(IERC20.approve, (address(this), amount)));
        calls[1] = Call(address(this), 0, abi.encodeCall(this.carry, (toChain, recipient, amount)));
    }

    /// Take the sender's dollars for delivery on `toChain`.
    function carry(uint256 toChain, address recipient, uint256 amount) external returns (uint256 sequence) {
        asset.safeTransferFrom(msg.sender, address(this), amount);
        sequence = ++sent;
        emit Carried(sequence, msg.sender, toChain, recipient, amount);
    }

    /// Land a send from `fromChain` here.
    function deliver(uint256 fromChain, uint256 sequence, address recipient, uint256 amount) external {
        if (msg.sender != operator) revert NotOperator();
        if (delivered[fromChain][sequence]) revert AlreadyDelivered();
        delivered[fromChain][sequence] = true;
        LocalUsd(address(asset)).mint(recipient, amount);
        emit Delivered(fromChain, sequence, recipient, amount);
    }
}
