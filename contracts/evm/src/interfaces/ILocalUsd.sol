// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// The dollar the local chain settles in: an ordinary token with six decimals
/// that anyone on this chain can mint, because it stands for the real one.
interface ILocalUsd {
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function decimals() external view returns (uint8);
    function totalSupply() external view returns (uint256);
    function balanceOf(address account) external view returns (uint256);
    function allowance(address owner, address spender) external view returns (uint256);
    function transfer(address to, uint256 value) external returns (bool);
    function approve(address spender, uint256 value) external returns (bool);
    function transferFrom(address from, address to, uint256 value) external returns (bool);

    /// Anyone, on this chain only.
    function mint(address to, uint256 amount) external;
}
