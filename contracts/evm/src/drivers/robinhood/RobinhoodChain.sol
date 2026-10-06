// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// The parts of the contracts on Robinhood Chain that the Robinhood driver
/// uses: Safe v1.4.1 and its proxy factory, Zodiac Roles v2 and the module
/// proxy factory, Uniswap v3 (SwapRouter02 and its pools), Chainlink's price
/// feeds and the stock tokens. Only the calls the driver makes or reads are
/// declared, with the signatures the deployed contracts have.

/// Safe v1.4.1.
interface ISafe {
    function setup(
        address[] calldata owners,
        uint256 threshold,
        address to,
        bytes calldata data,
        address fallbackHandler,
        address paymentToken,
        uint256 payment,
        address payable paymentReceiver
    ) external;

    function execTransaction(
        address to,
        uint256 value,
        bytes calldata data,
        uint8 operation,
        uint256 safeTxGas,
        uint256 baseGas,
        uint256 gasPrice,
        address gasToken,
        address payable refundReceiver,
        bytes calldata signatures
    ) external payable returns (bool success);

    function enableModule(address module) external;
    function isModuleEnabled(address module) external view returns (bool);
    function getOwners() external view returns (address[] memory);
    function getThreshold() external view returns (uint256);
}

interface ISafeProxyFactory {
    function createProxyWithNonce(
        address singleton,
        bytes calldata initializer,
        uint256 saltNonce
    ) external returns (address proxy);
    function proxyCreationCode() external pure returns (bytes memory);
}

/// Zodiac's factory for module proxies (minimal proxies by CREATE2).
interface IModuleProxyFactory {
    function deployModule(
        address masterCopy,
        bytes calldata initializer,
        uint256 saltNonce
    ) external returns (address proxy);
}

/// Zodiac Roles v2's condition tree, flattened: each node names its parent
/// by index, in breadth-first order.
struct ConditionFlat {
    uint8 parent;
    uint8 paramType;
    uint8 operator;
    bytes compValue;
}

/// Zodiac Roles v2: a module on the Safe that lets each role's members make
/// only the calls scoped for the role.
interface IRoles {
    function setUp(bytes calldata initParams) external;
    function owner() external view returns (address);
    function avatar() external view returns (address);
    function target() external view returns (address);
    function assignRoles(address module, bytes32[] calldata roleKeys, bool[] calldata memberOf) external;
    function scopeTarget(bytes32 roleKey, address targetAddress) external;
    function scopeFunction(
        bytes32 roleKey,
        address targetAddress,
        bytes4 selector,
        ConditionFlat[] calldata conditions,
        uint8 options
    ) external;
    function execTransactionWithRole(
        address to,
        uint256 value,
        bytes calldata data,
        uint8 operation,
        bytes32 roleKey,
        bool shouldRevert
    ) external returns (bool success);
}

/// What a Roles v2 custom condition answers.
interface ICustomCondition {
    function check(
        address to,
        uint256 value,
        bytes calldata data,
        uint8 operation,
        uint256 location,
        uint256 size,
        bytes12 extra
    ) external view returns (bool success, bytes32 reason);
}

/// Uniswap's SwapRouter02: the one swap the role allows.
interface ISwapRouter02 {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    function exactInputSingle(ExactInputSingleParams calldata params)
        external
        payable
        returns (uint256 amountOut);
}

interface IUniswapV3Pool {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function fee() external view returns (uint24);
    function factory() external view returns (address);
    function observe(uint32[] calldata secondsAgos)
        external
        view
        returns (int56[] memory tickCumulatives, uint160[] memory secondsPerLiquidityCumulativeX128s);
    function increaseObservationCardinalityNext(uint16 observationCardinalityNext) external;
}

interface IUniswapV3Factory {
    function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address);
}

/// Chainlink's price feed proxy.
interface IAggregatorV3 {
    function decimals() external view returns (uint8);
    function latestRoundData()
        external
        view
        returns (uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound);
}

/// What a Robinhood stock token adds to ERC-20.
interface IStockToken {
    /// Whether the issuer has paused the token's price oracle (corporate
    /// actions in progress): its feed is not to be trusted meanwhile.
    function oraclePaused() external view returns (bool);
}
