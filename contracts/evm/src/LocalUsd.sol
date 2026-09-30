// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ERC20} from "openzeppelin/token/ERC20/ERC20.sol";

/// The dollar the local chain settles in.
///
/// It has six decimals, like the dollar the platform settles in everywhere
/// else, and anyone can mint it, because a chain whose money is free is the
/// only kind worth developing against. `ILocalUsd` describes this surface for
/// the bindings; a test holds the two together.
contract LocalUsd is ERC20 {
    constructor() ERC20("Shonin development dollar", "USDC") {}

    function decimals() public pure override returns (uint8) {
        return 6;
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}
