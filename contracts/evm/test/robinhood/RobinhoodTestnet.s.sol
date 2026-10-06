// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {
    IModuleProxyFactory, IRoles, ISafeProxyFactory
} from "../../src/drivers/robinhood/RobinhoodChain.sol";
import {RobinhoodDriver} from "../../src/drivers/robinhood/RobinhoodDriver.sol";
import {IDriver} from "../../src/interfaces/IDriver.sol";
import {StationDouble} from "./StationDouble.sol";
import {Script, console} from "forge-std/Script.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

interface IMintable {
    function mint(address to, uint256 amount) external;
}

/// The Robinhood driver's custody on Robinhood Chain testnet, with the
/// chain's own Safe v1.4.1 and Zodiac Roles v2: a stand-in station gets a
/// Safe it alone owns and a role for the executor's key, moves dollars in
/// and out, and the executor's key makes the one kind of approval its role
/// allows. The testnet has no Chainlink feeds and no official Uniswap, so
/// the driver lists no stock token here and its router is a placeholder;
/// swaps, valuing and closing are shown on a fork of mainnet instead.
///
/// `forge script test/robinhood/RobinhoodTestnet.s.sol --rpc-url <46630> --broadcast`
/// with `STATION_KEY` (deploys, acts as the station) and `EXECUTOR_KEY`, and
/// `DOLLAR` (the stand-in dollar the testnet deployment uses).
contract RobinhoodTestnet is Script {
    address internal constant SAFE_SINGLETON = 0x29fcB43b46531BcA003ddC8FCB67FFE91900C762;
    address internal constant SAFE_FACTORY = 0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67;
    address internal constant ROLES_MASTERCOPY = 0x9646fDAD06d3e24444381f44362a3B0eB343D337;
    address internal constant MODULE_FACTORY = 0x000000000000aDdB49795b0f9bA5BC298cDda236;
    /// No Uniswap router exists on the testnet: the role is scoped to this
    /// address, which holds no code.
    address internal constant NO_ROUTER = 0x000000000000000000000000000000000000dEaD;
    bytes32 internal constant ROLE = "shonin-executor";

    function run() external {
        uint256 stationKey = vm.envUint("STATION_KEY");
        uint256 executorKey = vm.envUint("EXECUTOR_KEY");
        address dollar = vm.envAddress("DOLLAR");
        address executor = vm.addr(executorKey);

        vm.startBroadcast(stationKey);
        RobinhoodDriver driver = new RobinhoodDriver(
            RobinhoodDriver.Venue({
                dollar: IERC20(dollar),
                router: NO_ROUTER,
                safeSingleton: SAFE_SINGLETON,
                safeFactory: ISafeProxyFactory(SAFE_FACTORY),
                rolesMastercopy: ROLES_MASTERCOPY,
                moduleFactory: IModuleProxyFactory(MODULE_FACTORY),
                maxFeedAge: 4 days,
                twapWindow: 30 minutes
            }),
            new RobinhoodDriver.Listing[](0)
        );
        StationDouble station = new StationDouble(dollar, 500);
        IMintable(dollar).mint(address(station), 100e6);
        station.approveKey(IDriver(address(driver)), executor);
        station.deposit(IDriver(address(driver)), 60e6);
        vm.stopBroadcast();

        IRoles roles = IRoles(driver.rolesOf(address(station)));
        vm.startBroadcast(executorKey);
        roles.execTransactionWithRole(
            dollar, 0, abi.encodeCall(IERC20.approve, (NO_ROUTER, 1e6)), 0, ROLE, true
        );
        vm.stopBroadcast();

        vm.startBroadcast(stationKey);
        station.withdraw(IDriver(address(driver)), 20e6);
        vm.stopBroadcast();

        console.log("driver", address(driver));
        console.log("station stand-in", address(station));
        console.log("safe", driver.safeOf(address(station)));
        console.log("roles", address(roles));
    }
}
